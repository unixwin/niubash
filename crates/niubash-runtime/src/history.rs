//! Live, file-backed history for the interactive Reedline session.

use std::{
    collections::VecDeque,
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, MutexGuard},
    time::SystemTime,
};

use reedline::{
    CommandLineSearch, History, HistoryItem, HistoryItemId, HistorySessionId, ReedlineError,
    ReedlineErrorVariants, Result, SearchDirection, SearchQuery,
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

    /// Move the session's backing file to `new_path` (niubash#661: a
    /// mid-session `export HISTFILE=...`). See
    /// [`LiveFileBackedHistory::retarget`].
    pub(crate) fn retarget(&mut self, new_path: PathBuf) {
        self.inner.retarget(new_path);
    }

    /// The file this provider's history is backed by, when it has one.
    pub(crate) fn backing_file(&self) -> Option<PathBuf> {
        self.inner.backing_file()
    }

    /// The ledger shared with this provider's editor twin (niubash#569).
    pub(crate) fn ledger(&self) -> Arc<Mutex<SessionLedger>> {
        self.inner.ledger()
    }

    /// The HIST* settings cell shared with this provider's editor twin
    /// (niubash#646).
    pub(crate) fn settings(&self) -> Arc<Mutex<HistorySettings>> {
        self.inner.settings()
    }
}

/// The GNU history variables that govern the REPL record/list/file faces,
/// snapshotted from the shell environment by the host
/// (`Shell::refresh_history_settings`, niubash#646).
///
/// GNU reads these variables at the moment of the operation they govern
/// (bashhist.c: check_history_control / history_should_ignore at record
/// time, sv_histsize for the list, HISTFILESIZE at the session-end
/// write), so the host refreshes this cell at every prompt boundary and
/// once more after the EXIT trap, and the history objects consult the
/// snapshot instead of reaching back into the executor. Values are kept
/// raw (the two pattern/control strings) or pre-parsed with the engine's
/// own mapping (`SessionHistory::size_limit`, variables.c sv_histsize).
///
/// HISTTIMEFORMAT is deliberately absent: it has no consumer on this
/// side of the host/engine boundary. The engine renders timestamps from
/// its own session list (`SessionHistory::timestamps`), the
/// `HistoryProvider` contract carries plain entry strings only, and
/// reedline's file format (one encoded line per entry, no stamp lines)
/// cannot persist the `#<epoch>` side lines GNU writes — so a host-side
/// value would be a write-only reader. The REPL timestamp face waits on
/// the provider contract growing a stamp channel (niubash#622).
#[derive(Clone)]
pub(crate) struct HistorySettings {
    /// Raw `HISTCONTROL` value (colon-separated tokens).
    pub(crate) histcontrol: String,
    /// Raw `HISTIGNORE` value (colon-separated fnmatch patterns).
    pub(crate) histignore: String,
    /// `HISTSIZE` through `SessionHistory::size_limit`: `Some(n)` stifles
    /// the list view to its last `n` entries; `None` is unstifled.
    pub(crate) histsize: Option<usize>,
    /// `HISTFILESIZE` through the same mapping: `Some(n)` truncates the
    /// history file to its last `n` lines at the session-end write and
    /// after `history -w`; `None` never truncates.
    pub(crate) histfilesize: Option<usize>,
    /// The `histappend` shopt state. GNU's exit write consults it
    /// (bashhist.c: force_append_history selects the append branch of
    /// maybe_save_shell_history); the session-end flush does the same
    /// (see `LiveFileBackedHistory::flush_at_session_end`).
    pub(crate) histappend: bool,
    /// The `history` shell option (`set -o history`) state
    /// (niubash#660). GNU records a typed line only while the option is
    /// on (bashhist.c: remember_on_history gates maybe_add_history);
    /// the editor instance's record funnel consults this snapshot and
    /// drops the line while it is off. `history -s` is NOT gated: GNU
    /// pushes `-s` operands through add_history unconditionally
    /// (builtins/history.def), verified against bash 5.3.20.
    pub(crate) history_enabled: bool,
}

impl Default for HistorySettings {
    fn default() -> Self {
        Self {
            histcontrol: String::new(),
            histignore: String::new(),
            histsize: None,
            histfilesize: None,
            histappend: false,
            // GNU interactive shells default the option ON; a snapshot
            // that was never refreshed must fail open to recording, not
            // silently swallow a session's lines.
            history_enabled: true,
        }
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
        // niubash#646: GNU's write_history truncates the written file
        // to HISTFILESIZE lines (histfile.c history_write); with one
        // line per entry, that is the entry list's tail, and the
        // ledger reset below counts what was actually written.
        let entries = match self.inner.settings().lock() {
            Ok(settings) => match settings.histfilesize {
                Some(limit) if entries.len() > limit => entries[entries.len() - limit..].to_vec(),
                _ => entries,
            },
            Err(_) => entries,
        };
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
        // niubash#646: loads bypass the record filters (GNU's read
        // path applies no HISTCONTROL/HISTIGNORE filtering), so these
        // go through the unfiltered save, not the provider append.
        for line in missing {
            self.inner
                .save_unfiltered(HistoryItem::from_command_line(line))
                .map_err(|error| io::Error::other(error.to_string()))?;
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
    history: HistoryStore,
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
    /// The HIST* settings cell shared with the twin instance
    /// (niubash#646); the same `Arc` is held by
    /// [`LiveFileBackedHistory::settings`].
    settings: Arc<Mutex<HistorySettings>>,
    /// Latest `HISTSIZE` snapshot applied to the list view, refreshed
    /// from `settings` by `sync_view` on every query: the view keeps
    /// only the last this many raw entries, on top of the clear hide
    /// (the fold happens in `view_bounds`).
    view_histsize: Option<usize>,
}

/// Host-owned history entry store: the entry list and its on-disk
/// accounting, held inside `HistoryState` in place of reedline's
/// `FileBackedHistory`.
///
/// Why this exists (niubash redundancy cleanup, audit item H-04):
/// reedline's only write path is `sync()`, which re-reads and re-merges
/// the WHOLE history file on every call — so every recorded line cost a
/// full-file read even though the wrapper already tracks the file with
/// its own signature gate and session ledger. The store keeps reedline's
/// exact entry semantics — the same record filter, capacity rule,
/// search-window arithmetic, unsupported-feature errors, and merge
/// shape, transcribed from reedline 0.52.1's `file_backed.rs` — but owns
/// `len_on_disk` itself, so the save path appends just the unwritten
/// tail (`flush`) and the file is re-read only where the wrapper's
/// signature gate saw it change (`merge_from_disk`, and the reloads
/// that rebuild the store from the file).
///
/// Invariant the flush fast path relies on: the file's first
/// `len_on_disk` lines are exactly `entries[..len_on_disk]`; foreign
/// appends land after them and are folded in by the next merge, which
/// re-derives the list from the file. The one event that breaks the
/// prefix is the capacity pop retiring a head entry that is already on
/// disk; `save` flags it (`disk_prefix_stale`) and the next flush takes
/// the merge path instead, exactly as reedline's sync would have.
struct HistoryStore {
    capacity: usize,
    entries: VecDeque<String>,
    /// How many leading entries the backing file already holds.
    len_on_disk: usize,
    /// A capacity pop retired an on-disk head entry: the file's prefix
    /// no longer matches `entries`, so the next flush must merge rather
    /// than append.
    disk_prefix_stale: bool,
}

impl HistoryStore {
    /// In-memory store (no backing file). Callers clamp `capacity` the
    /// way `in_memory_history` does.
    fn new(capacity: usize) -> Self {
        Self {
            capacity,
            entries: VecDeque::new(),
            len_on_disk: 0,
            disk_prefix_stale: false,
        }
    }

    /// Load the backing file the way `FileBackedHistory::with_file`
    /// does: a fresh store merged against the file. Construction fails
    /// exactly where reedline's does (capacity overflow, unreadable or
    /// undecodable file), so the caller's degrade-to-memory fallback
    /// keeps its current shape.
    fn load_from_file(capacity: usize, path: &Path) -> io::Result<Self> {
        if capacity == usize::MAX {
            return Err(io::Error::other(
                "History capacity too large to be addressed safely",
            ));
        }
        let mut store = Self::new(capacity);
        store.merge_from_disk(path)?;
        Ok(store)
    }

    /// Reedline's entry construction: this history stores no info
    /// except the command line (file_backed.rs `construct_entry`).
    fn construct_entry(id: Option<HistoryItemId>, command_line: String) -> HistoryItem {
        HistoryItem {
            id,
            start_timestamp: None,
            command_line,
            session_id: None,
            hostname: None,
            cwd: None,
            duration: None,
            exit_status: None,
            more_info: None,
        }
    }

    /// Record one entry (file_backed.rs `save`): an entry identical to
    /// the previous one, an empty entry, or any entry at capacity zero
    /// is not appended; at capacity the oldest entry is retired first
    /// (and the on-disk accounting follows it — see the struct docs).
    fn save(&mut self, item: HistoryItem) -> HistoryItem {
        let entry = item.command_line;
        let entry_id =
            if (self.entries.back() != Some(&entry)) && !entry.is_empty() && self.capacity > 0 {
                if self.entries.len() == self.capacity {
                    self.entries.pop_front();
                    if self.len_on_disk > 0 {
                        // The retired head was already on disk: the
                        // file's prefix no longer matches `entries`.
                        self.disk_prefix_stale = true;
                    }
                    self.len_on_disk = self.len_on_disk.saturating_sub(1);
                }
                self.entries.push_back(entry.clone());
                Some(HistoryItemId::new((self.entries.len() - 1) as i64))
            } else {
                None
            };
        Self::construct_entry(entry_id, entry)
    }

    fn load(&self, id: HistoryItemId) -> Result<HistoryItem> {
        Ok(Self::construct_entry(
            Some(id),
            self.entries
                .get(id.0 as usize)
                .ok_or(ReedlineError(ReedlineErrorVariants::OtherHistoryError(
                    "Item does not exist",
                )))?
                .clone(),
        ))
    }

    fn count(&self, query: SearchQuery) -> Result<i64> {
        // file_backed.rs: "todo: this could be done cheaper"
        Ok(self.search(query)?.len() as i64)
    }

    /// The reader's own filter machinery (file_backed.rs `search`),
    /// transcribed: time filters and extra-info filters are unsupported
    /// and error, id bounds are exclusive outside / inclusive inside,
    /// and the command-line filter is Prefix/Substring/Exact plus the
    /// `not_command_line` exclusion.
    fn search(&self, query: SearchQuery) -> Result<Vec<HistoryItem>> {
        self.search_filtered(query, None)
    }

    /// [`Self::search`] with the query's `not_command_line` exclusion
    /// supplied out of band. The field is `pub(crate)` to reedline, so
    /// a query that carries it (only reedline's own `HistoryCursor`
    /// builds one) cannot be read back from outside the crate; the
    /// wrapper reconstructs the value from the cursor's closed query
    /// shape instead (see `LiveFileBackedHistory::cursor_not_command_line`).
    fn search_filtered(
        &self,
        query: SearchQuery,
        not_command_line: Option<&str>,
    ) -> Result<Vec<HistoryItem>> {
        if query.start_time.is_some() || query.end_time.is_some() {
            return Err(ReedlineError(
                ReedlineErrorVariants::HistoryFeatureUnsupported {
                    history: "FileBackedHistory",
                    feature: "filtering by time",
                },
            ));
        }

        if query.filter.hostname.is_some()
            || query.filter.cwd_exact.is_some()
            || query.filter.cwd_prefix.is_some()
            || query.filter.exit_successful.is_some()
        {
            return Err(ReedlineError(
                ReedlineErrorVariants::HistoryFeatureUnsupported {
                    history: "FileBackedHistory",
                    feature: "filtering by extra info",
                },
            ));
        }
        let (min_id, max_id) = {
            let start = query.start_id.map(|e| e.0);
            let end = query.end_id.map(|e| e.0);
            if let SearchDirection::Backward = query.direction {
                (end, start)
            } else {
                (start, end)
            }
        };
        // add one to make it inclusive
        let min_id = min_id.map(|e| e + 1).unwrap_or(0);
        // subtract one to make it inclusive
        let max_id = max_id
            .map(|e| e - 1)
            .unwrap_or(self.entries.len() as i64 - 1);
        if max_id < 0 || min_id > self.entries.len() as i64 - 1 {
            return Ok(vec![]);
        }
        let intrinsic_limit = max_id - min_id + 1;
        let limit = if let Some(given_limit) = query.limit {
            std::cmp::min(intrinsic_limit, given_limit) as usize
        } else {
            intrinsic_limit as usize
        };
        let filter = |(idx, cmd): (usize, &String)| {
            if !match &query.filter.command_line {
                Some(CommandLineSearch::Prefix(p)) => cmd.starts_with(p),
                Some(CommandLineSearch::Substring(p)) => cmd.contains(p),
                Some(CommandLineSearch::Exact(p)) => cmd == p,
                None => true,
            } {
                return None;
            }
            if let Some(str) = not_command_line {
                if cmd == str {
                    return None;
                }
            }
            Some(Self::construct_entry(
                Some(HistoryItemId::new(idx as i64)),
                cmd.to_string(),
            ))
        };

        let iter = self
            .entries
            .iter()
            .enumerate()
            .skip(min_id as usize)
            .take(intrinsic_limit as usize);
        if let SearchDirection::Backward = query.direction {
            Ok(iter.rev().filter_map(filter).take(limit).collect())
        } else {
            Ok(iter.filter_map(filter).take(limit).collect())
        }
    }

    fn update(&self) -> Result<()> {
        Err(ReedlineError(
            ReedlineErrorVariants::HistoryFeatureUnsupported {
                history: "FileBackedHistory",
                feature: "updating entries",
            },
        ))
    }

    fn delete(&self) -> Result<()> {
        Err(ReedlineError(
            ReedlineErrorVariants::HistoryFeatureUnsupported {
                history: "FileBackedHistory",
                feature: "removing entries",
            },
        ))
    }

    /// Empty the in-memory list and reset the on-disk accounting
    /// (file_backed.rs `clear`'s memory half). The file side effect
    /// reedline's `clear` also has — removing the backing file — is the
    /// callers' own step, in the same order (see `clear_memory` and
    /// the `History::clear` impl).
    fn clear_entries(&mut self) {
        self.entries.clear();
        self.len_on_disk = 0;
        self.disk_prefix_stale = false;
    }

    /// Write the unwritten tail to the backing file — reedline `sync`'s
    /// write half with its read half removed. In the steady state the
    /// file gains exactly `entries[len_on_disk..]`, appended; when the
    /// on-disk prefix may no longer match (capacity pop), this takes
    /// the merge path, which is reedline's sync in full.
    fn flush(&mut self, path: &Path) -> io::Result<()> {
        if self.disk_prefix_stale {
            return self.merge_from_disk(path);
        }
        if let Some(base_dir) = path.parent() {
            fs::create_dir_all(base_dir)?;
        }
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?;
        for line in self.entries.range(self.len_on_disk..) {
            writeln!(file, "{}", encode_history_line(line))?;
        }
        file.flush()?;
        self.len_on_disk = self.entries.len();
        Ok(())
    }

    /// Reedline `sync`'s merge, transcribed (file_backed.rs:199-287):
    /// read the whole file, fold this store's unwritten tail onto it,
    /// drop the foreign head when the total exceeds capacity (the file
    /// is rewritten to the kept lines), otherwise append the tail.
    /// Afterwards the file holds exactly `entries` and
    /// `len_on_disk == entries.len()`. This is the only store operation
    /// that reads the file; it runs where reedline's sync used to run
    /// for a merge (construction, the session-end flush's stale-prefix
    /// fallback) — the per-operation refresh goes through the
    /// signature gate and rebuilds from the file instead (see
    /// `refresh_if_stale`).
    fn merge_from_disk(&mut self, path: &Path) -> io::Result<()> {
        use std::io::{BufRead, BufReader};

        // The unwritten entries
        let start = self.len_on_disk.min(self.entries.len());
        let own_entries: Vec<String> = self.entries.range(start..).cloned().collect();

        if let Some(base_dir) = path.parent() {
            fs::create_dir_all(base_dir)?;
        }

        let file = fs::OpenOptions::new()
            .create(true)
            .write(true)
            .read(true)
            .truncate(false)
            .open(path)?;
        let mut foreign_entries: VecDeque<String> = BufReader::new(&file)
            .lines()
            .map(|line| line.map(|l| decode_history_line(&l)))
            .collect::<io::Result<VecDeque<_>>>()?;
        drop(file);

        let truncate = if foreign_entries.len() + own_entries.len() > self.capacity {
            foreign_entries = foreign_entries.split_off(
                foreign_entries
                    .len()
                    .saturating_sub(self.capacity.saturating_sub(own_entries.len())),
            );
            true
        } else {
            false
        };

        if truncate {
            let mut kept: Vec<String> = foreign_entries.iter().cloned().collect();
            kept.extend(own_entries.iter().cloned());
            write_entries_staged(path, &kept)?;
        } else if !own_entries.is_empty() {
            let mut file = fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)?;
            for line in &own_entries {
                writeln!(file, "{}", encode_history_line(line))?;
            }
            file.flush()?;
        }

        foreign_entries.extend(own_entries);
        self.entries = foreign_entries;
        self.len_on_disk = self.entries.len();
        self.disk_prefix_stale = false;
        Ok(())
    }
}

/// A Reedline file history that is visible across concurrently running shells.
///
/// The entry store (`HistoryStore`) is host-owned: saves append their
/// line to the file immediately, and the wrapper reloads the file
/// before queries when another process has changed it, so concurrently
/// running shells see each other's lines without waiting for an exit.
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
    /// Shared with the provider/editor twin over the same file
    /// (niubash#646): the HIST* settings snapshot the host refreshes.
    settings: Arc<Mutex<HistorySettings>>,
    /// niubash#660: the editor instance (the one reedline records typed
    /// lines into) carries this gate; its record funnel drops a line
    /// while the settings snapshot says the `history` shell option is
    /// off. The provider instance never carries it: `history -s`
    /// reaches the list through the provider's `append`, and GNU adds
    /// `-s` operands even with the option off.
    gate_on_history_option: bool,
}

/// Clamp so the memory-only fallback is always constructible (the
/// file-backed constructor rejects `capacity == usize::MAX`).
fn in_memory_history(capacity: usize) -> HistoryStore {
    HistoryStore::new(capacity.min(usize::MAX - 1))
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
        Self::with_shared_ledger_and_settings(
            capacity,
            path,
            ledger,
            Arc::new(Mutex::new(HistorySettings::default())),
        )
    }

    /// Build the live history over a ledger AND a settings cell shared
    /// with a twin instance (niubash#569/#646): the host installs the
    /// provider with both cells, and the editor twin is built over the
    /// same pair so record-time filters and list stifling agree.
    pub(crate) fn with_shared_ledger_and_settings(
        capacity: usize,
        path: PathBuf,
        ledger: Arc<Mutex<SessionLedger>>,
        settings: Arc<Mutex<HistorySettings>>,
    ) -> Self {
        // On Windows, ensure the history file has accessible ACLs before opening.
        // Codex sandbox and similar environments may create files with restrictive
        // permissions that block read access (os error 5).
        ensure_history_file_accessible(&path);

        let state = match HistoryStore::load_from_file(capacity, &path) {
            Ok(history) => {
                let mut state = HistoryState {
                    history,
                    signature: FileSignature::default(),
                    path: Some(path.clone()),
                    warned_io_failure: false,
                    hidden_prefix: 0,
                    applied_view_gen: 0,
                    settings: Arc::clone(&settings),
                    view_histsize: None,
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
                    settings: Arc::clone(&settings),
                    view_histsize: None,
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
            settings,
            gate_on_history_option: false,
        }
    }

    /// Mark this instance as the editor's recording surface
    /// (niubash#660): its saves are dropped while the `history` shell
    /// option is off. Called by `build_line_editor` on the instance it
    /// hands to reedline; provider instances are never marked.
    pub(crate) fn set_history_option_gate(&mut self) {
        self.gate_on_history_option = true;
    }

    /// The ledger shared with this history's twin instance, if any.
    pub(crate) fn ledger(&self) -> Arc<Mutex<SessionLedger>> {
        Arc::clone(&self.ledger)
    }

    /// The HIST* settings cell shared with this history's twin instance
    /// (niubash#646).
    pub(crate) fn settings(&self) -> Arc<Mutex<HistorySettings>> {
        Arc::clone(&self.settings)
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
        // in-memory view with the latest file contents. The flush is the
        // store's append-only write (no read); the reload below is the
        // single full read this refresh pays — reedline's sync used to
        // read the whole file for the flush as well, and its merge
        // result was then discarded for this same reload (audit item
        // H-03).
        if let Err(error) = state.history.flush(&path) {
            warn_io_failure(state, "history flush before reload failed", error);
        }
        match HistoryStore::load_from_file(capacity, &path) {
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
        // niubash#646: the HISTSIZE stifle tracks the settings cell on
        // every query, not just on view generations — GNU reads the
        // variable at the moment of the operation, and the host
        // refreshes the cell at prompt boundaries.
        if let Ok(settings) = state.settings.lock() {
            state.view_histsize = settings.histsize;
        }
    }

    /// The hidden prefix clamped to the reader's current length, plus
    /// that length. If the file shrank below the clear point (a foreign
    /// rewrite), everything currently in the list predates the clear.
    ///
    /// niubash#646: the HISTSIZE stifle folds in here — the view starts
    /// past the LATER of the clear hide and the stifle, so every list
    /// face (search/count/load, the provider's entries) sees the same
    /// last-`HISTSIZE` window GNU's stifled list would show. The raw
    /// list and the file keep everything; only the view is stifled,
    /// which is also what GNU's exit append produces on disk.
    fn view_bounds(state: &HistoryState) -> (usize, usize) {
        let raw_len = state
            .history
            .count(SearchQuery::all_that_contain_rev(String::new()))
            .map(|count| count as usize)
            .unwrap_or(0);
        let stifle = state
            .view_histsize
            .map(|limit| raw_len.saturating_sub(limit))
            .unwrap_or(0);
        (state.hidden_prefix.max(stifle).min(raw_len), raw_len)
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
    fn view_search(
        state: &HistoryState,
        query: SearchQuery,
        not_command_line: Option<&str>,
    ) -> Result<Vec<HistoryItem>> {
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
            .search_filtered(scan, not_command_line)?
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

    /// Reconstruct a query's `not_command_line` exclusion, which the
    /// store cannot read off the query itself (the field is
    /// `pub(crate)` to reedline). reedline's `HistoryCursor` is the
    /// only producer of such queries (cursor.rs `get_search_filter`,
    /// with `skip_dupes` hardcoded on): its shape is closed — limit 1,
    /// `start_id` set to the entry it is currently showing, no end id
    /// and no time bounds — and the excluded value is by construction
    /// that entry's command line, so it can be re-derived from the
    /// store at the same id (view ids re-based by the hidden prefix,
    /// the same shift `History::load` applies). Every other query
    /// shape maps to `None`, which is exactly what every public
    /// `SearchFilter` constructor produces.
    fn cursor_not_command_line(state: &HistoryState, query: &SearchQuery) -> Option<String> {
        if query.limit != Some(1)
            || query.end_id.is_some()
            || query.start_time.is_some()
            || query.end_time.is_some()
        {
            return None;
        }
        let start_id = query.start_id?;
        let (hidden, _) = Self::view_bounds(state);
        state
            .history
            .load(HistoryItemId::new(start_id.0 + hidden as i64))
            .ok()
            .map(|item| item.command_line)
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
        let synced = match state.path.clone() {
            Some(path) => match state.history.flush(&path) {
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
        match HistoryStore::load_from_file(capacity, &path) {
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
    /// This cannot be built from per-entry saves: the store's `save`
    /// (reedline's record filter, transcribed) drops an entry identical
    /// to the one before it ("only saves a value if it's different than the last
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
        //
        // niubash#595: stream the staged file instead of building one
        // giant String first — a 100k-entry list used to allocate the
        // whole file twice over (the joined String, then the write).
        // Byte-identical output: each entry's `\n` segments joined by
        // `<\n>`, terminated by `\n` (so `""` writes a bare newline and
        // `"\n"` writes `<\n>\n`, exactly as the old join did).
        let entry_count = entries.len();
        let staged = write_entries_staged(&staged_path, &entries);
        let rebuilt = staged.and_then(|()| HistoryStore::load_from_file(capacity, &staged_path));
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
                    ledger.reset(entry_count);
                }
                Self::sync_view(state, &ledger);
                Ok(())
            }
            Err(error) => {
                if state.path.is_some() {
                    return Err(ReedlineError::from(error));
                }
                // Memory-only staging failed: history is auxiliary state,
                // so degrade to the clear-and-save rewrite (which cannot
                // hold consecutive duplicates) instead of failing. (The
                // store is memory-only here, so clearing it has no file
                // side effect to reproduce.)
                state.history.clear_entries();
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
    /// The store's clear only empties the list; the wrapper removes the
    /// backing file itself (reedline's clear did both in one call), so
    /// the file bytes are snapshotted and restored around that removal
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
        // niubash#642: the snapshot's decoded lines are what the ledger
        // captures as the cleared base set — post-refresh the reader
        // equals the file, so the snapshot IS the file the clear sees.
        let snapshot_lines: Vec<String> = snapshot
            .as_deref()
            .map(|bytes| {
                String::from_utf8_lossy(bytes)
                    .lines()
                    .map(decode_history_line)
                    .collect()
            })
            .unwrap_or_default();
        // Reedline's `clear` emptied the list AND removed the backing
        // file; the store only owns the list, so the removal is spelled
        // out here — the snapshot restore below recreates the file with
        // its original bytes, exactly as it did around reedline's clear.
        state.history.clear_entries();
        if let Some(path) = state.path.clone() {
            std::fs::remove_file(&path).map_err(ReedlineError::from)?;
        }
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
            ledger.note_cleared(view_len_at_clear, &snapshot_lines);
        }
        Self::sync_view(state, &ledger);
        outcome
    }

    /// The file this history is backed by, when it has one.
    pub(crate) fn backing_file(&self) -> Option<PathBuf> {
        self.state.lock().ok().and_then(|state| state.path.clone())
    }

    /// Move this history's backing file to `new_path` mid-session
    /// (niubash#661: the user assigned `HISTFILE` a new value and the
    /// host's prompt-boundary refresh noticed). GNU reads HISTFILE at
    /// the moment of each operation it governs, so the session stream
    /// follows the live value: lines recorded so far settle into the
    /// old file (niu syncs live — they belong to the period it was the
    /// file of record), and from here on saves and the session-end
    /// write target the new file.
    ///
    /// The switch re-bases the store's on-disk accounting on the new
    /// file by staging the current list view into it — the same
    /// rebuild `replace_all` performs, ledger reset included — which
    /// is what keeps the store's "file prefix == list prefix"
    /// invariant true for the new path: merely repointing `path`
    /// would make the flush fast path append only the tail past the
    /// OLD file's line count and silently skip earlier session lines.
    /// A side effect of staging is that the new file receives the list
    /// at switch time rather than at exit; under niu's live-sync model
    /// the end state is the one GNU's exit write produces. The switch
    /// is atomic on failure: if the new file cannot be staged and
    /// loaded back, the old backing stays in place and the failure is
    /// reported once, like every history I/O failure.
    pub(crate) fn retarget(&mut self, new_path: PathBuf) {
        if self.backing_file().as_deref() == Some(new_path.as_path()) {
            return;
        }
        ensure_history_file_accessible(&new_path);
        let capacity = self.capacity;
        // Clone the ledger handle before taking the state lock (see save).
        let ledger = Arc::clone(&self.ledger);
        // The list the new file must hold: the post-clear view, the
        // same list the provider's `entries()` reports.
        let entries: Vec<String> = self
            .search(SearchQuery::all_that_contain_rev(String::new()))
            .map(|items| {
                items
                    .into_iter()
                    .rev()
                    .map(|item| item.command_line)
                    .collect()
            })
            .unwrap_or_default();
        let Ok(state) = self.state.get_mut() else {
            return;
        };
        // Settle the unwritten tail into the file of record so far.
        Self::sync_state(state);
        let staged = write_entries_staged(&new_path, &entries)
            .and_then(|()| HistoryStore::load_from_file(capacity, &new_path));
        match staged {
            Ok(history) => {
                state.history = history;
                state.signature = file_signature_lossy(&new_path, state);
                state.path = Some(new_path);
                // The staged file IS the list now (niubash#614: the
                // reset also ends any clear hide).
                if let Ok(mut ledger) = ledger.lock() {
                    ledger.reset(entries.len());
                }
                Self::sync_view(state, &ledger);
            }
            Err(error) => {
                warn_io_failure(
                    state,
                    &format!(
                        "history file {} could not be adopted; keeping the previous history file",
                        new_path.display()
                    ),
                    error,
                );
            }
        }
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
        let (unsynced, cleared, cleared_base, base_lines) = match ledger.lock() {
            Ok(ledger) => (
                ledger
                    .pending
                    .iter()
                    .filter(|p| !p.synced)
                    .map(|p| p.text.clone())
                    .collect::<Vec<String>>(),
                ledger.cleared.clone(),
                ledger.cleared_base.clone(),
                ledger.base_lines,
            ),
            Err(_) => (Vec::new(), Vec::new(), Vec::new(), 0),
        };
        // niubash#646: the session-end write consults the final
        // histappend/HISTFILESIZE values — the host refreshed the
        // settings cell once more after the EXIT trap ran, so values
        // the trap itself set are the ones that govern the write.
        let (histappend, histfilesize) = state
            .settings
            .lock()
            .map(|settings| (settings.histappend, settings.histfilesize))
            .unwrap_or((false, None));
        if entries.is_empty() && cleared.is_empty() && cleared_base.is_empty() {
            // Only an existing file needs truncating; a session without
            // history must not create an empty one. (Pinned by
            // session_end_write_carries_a_cleared_list_to_the_file: a
            // cleared list with nothing pending truncates, exactly as
            // before.) When either cleared set is non-empty the file
            // is reconciled by removal below instead — truncating
            // would delete seeds and foreign lines, which
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
        // Write them THROUGH the store, not by raw appends: the store's
        // unwritten tail (entries past its on-disk accounting) holds
        // exactly the lines whose live sync failed, and its flush
        // advances that accounting as it writes them, so no later write
        // can place the same lines a second time — the duplication
        // session_end_write_appends_missing_entries_without_duplicates
        // catches where the read-only leg behaves (Windows CI; the
        // local uid-0 artifact masks the leg and hides this one).
        let mut appended_ok = unsynced.is_empty();
        let mut file_changed = false;
        if !unsynced.is_empty() {
            match state.history.flush(&path) {
                Ok(()) => {
                    appended_ok = true;
                    file_changed = true;
                }
                Err(error) => {
                    warn_io_failure(state, "history file append at session end failed", error);
                }
            }
        }
        // (b) Remove exactly the lines this session cleared. The
        // session's own lines (`cleared`) go as a forward subsequence
        // from the ledger base: lines before the base predate the
        // session, and lines committed by -a/-w left the pending set
        // before -c, so both survive. The base lines the session
        // cleared (`cleared_base`, niubash#642) go as a second pass
        // confined to the base window — the file's first `base_lines`
        // lines, where the base sat at startup; the first pass removes
        // nothing before the base, so the window is intact — and a
        // foreign line appended later is never eaten, even when it
        // shares the cleared text.
        //
        // With histappend on, GNU's exit write takes the append branch
        // (bashhist.c maybe_save_shell_history + force_append_history)
        // and `history -c` has already zeroed the session-line counter
        // (bash_clear_history): only post-clear lines are appended and
        // the base lines stay. niu's live sync HAS written the cleared
        // own lines to the file mid-session, so the first pass still
        // runs in append mode — it is what makes the end state match
        // GNU's (the niubash#569 grid: seeds plus post-clear lines).
        // Only the base pass is append-mode-exempt: removing cleared
        // base lines is #642's stronger-than-GNU contract, and it
        // belongs to the overwrite-flavored default mode.
        let mut removed_ok = cleared.is_empty() && (histappend || cleared_base.is_empty());
        if !removed_ok {
            match std::fs::read_to_string(&path) {
                Ok(contents) => {
                    let total = contents.lines().count();
                    let file_lines: Vec<String> = contents.lines().map(|l| l.to_string()).collect();
                    let mut kept = file_lines;
                    if !cleared.is_empty() {
                        kept = remove_cleared_lines(kept, &cleared, base_lines);
                    }
                    if !histappend && !cleared_base.is_empty() {
                        let window = base_lines.min(kept.len());
                        let tail: Vec<String> = kept.drain(window..).collect();
                        let mut prefix = remove_cleared_lines(kept, &cleared_base, 0);
                        prefix.extend(tail);
                        kept = prefix;
                    }
                    let removed = total - kept.len();
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
        // (c) niubash#646: HISTFILESIZE truncates the file to its last
        // lines once the write settles — bashhist.c's
        // maybe_save_shell_history ends with sv_histsize(
        // "HISTFILESIZE"), and histfile.c applies it to the file tail.
        if let Some(limit) = histfilesize {
            match std::fs::read_to_string(&path) {
                Ok(contents) => {
                    let lines: Vec<&str> = contents.lines().collect();
                    if lines.len() > limit {
                        let kept = &lines[lines.len() - limit..];
                        let rewritten = if kept.is_empty() {
                            String::new()
                        } else {
                            kept.join("\n") + "\n"
                        };
                        match std::fs::write(&path, rewritten) {
                            Ok(()) => {
                                file_changed = true;
                            }
                            Err(error) => {
                                warn_io_failure(
                                    state,
                                    "history file size truncation at session end failed",
                                    error,
                                );
                            }
                        }
                    }
                }
                Err(error) => {
                    warn_io_failure(
                        state,
                        "history file read for size truncation at session end failed",
                        error,
                    );
                }
            }
        }
        if file_changed {
            // Rebuild the reader from the reconciled file so its
            // in-memory list matches the file (step (b) may have removed
            // lines the reader still holds). The rebuilt store's
            // on-disk accounting covers its whole list — step (a)
            // flushed it, or there was nothing pending — so no later
            // write can re-place those lines. The rebuild cannot
            // resurrect cleared lines — they are already gone from the
            // file — and seeds reappearing in memory are committed file
            // lines, harmless at session end.
            if let Ok(history) = HistoryStore::load_from_file(capacity, &path) {
                state.history = history;
            }
        }
        if let Ok(mut ledger) = ledger.lock() {
            ledger.settle(appended_ok, removed_ok, count_file_lines(&path));
        }
        state.signature = file_signature_lossy(&path, state);
    }

    /// Save one entry without the niubash#646 record filters. Only
    /// file loads use this (read_new_history's fallback): GNU's read
    /// path applies no HISTCONTROL/HISTIGNORE filtering to lines it
    /// loads, exactly like its `-r`/`-n` list semantics.
    pub(crate) fn save_unfiltered(&mut self, item: HistoryItem) -> Result<HistoryItem> {
        self.save_entry(item, false)
    }

    fn save_entry(&mut self, item: HistoryItem, apply_filters: bool) -> Result<HistoryItem> {
        let capacity = self.capacity;
        // Clone the ledger handle before taking the state lock: the lock
        // borrows `self` mutably, so the handle must already be owned.
        let ledger = Arc::clone(&self.ledger);
        let gate_on_history_option = self.gate_on_history_option;
        let state = self.lock_mut()?;
        Self::refresh_if_stale(capacity, state);
        let text = item.command_line.clone();
        if text.is_empty() {
            // Reedline drops empty saves, and GNU records no empty lines
            // either — keep that one filter.
            return Ok(item);
        }

        // niubash#646: the GNU record-time filters, in bashhist.c's
        // order — HISTIGNORE (history_should_ignore) is consulted
        // before the HISTCONTROL bits (check_history_control:
        // ignorespace, then ignoredups/erasedups against the previous
        // entry). A filtered line is not an error: the caller gets its
        // item back and the line simply never reaches the list, the
        // file, or the ledger — GNU adds nothing for it either. Every
        // record path funnels through here: typed lines via reedline,
        // and `history -s` via the provider's append (GNU -s goes
        // through check_add_history the same way). Only file loads
        // bypass the filters (save_entry with apply_filters = false,
        // used by read_new_history's fallback), exactly like GNU's
        // read path.
        if apply_filters && gate_on_history_option {
            // niubash#660: GNU records a typed line only while the
            // `history` shell option is on (bashhist.c:
            // remember_on_history gates maybe_add_history, ahead of
            // every HISTCONTROL/HISTIGNORE filter). Only the editor
            // instance carries this gate; the provider instance does
            // not, so `history -s` (provider append) keeps adding
            // while the option is off, exactly as GNU's -s does. A
            // gated line is not an error: it simply never reaches the
            // list, the file, or the ledger.
            let history_enabled = state
                .settings
                .lock()
                .map(|settings| settings.history_enabled)
                .unwrap_or(true);
            if !history_enabled {
                return Ok(item);
            }
        }
        if apply_filters {
            let settings = state
                .settings
                .lock()
                .map(|settings| settings.clone())
                .unwrap_or_default();
            if !settings.histignore.is_empty() || !settings.histcontrol.is_empty() {
                Self::sync_view(state, &ledger);
                let (hidden, _) = Self::view_bounds(state);
                let view_entries: Vec<String> = state
                    .history
                    .search(SearchQuery::all_that_contain_rev(String::new()))
                    .map(|items| {
                        items
                            .into_iter()
                            .rev()
                            .map(|item| item.command_line)
                            .collect::<Vec<String>>()
                    })
                    .unwrap_or_default();
                let view_entries = &view_entries[hidden.min(view_entries.len())..];
                let last_entry = view_entries.last().map(String::as_str);
                if history_should_ignore(&text, last_entry.unwrap_or(""), &settings.histignore) {
                    return Ok(item);
                }
                let control = HistControl::parse(&settings.histcontrol);
                if control.ignore_space && (text.starts_with(' ') || text.starts_with('\t')) {
                    return Ok(item);
                }
                if control.ignore_dups && last_entry == Some(text.as_str()) {
                    return Ok(item);
                }
                if control.erase_dups && view_entries.iter().any(|entry| entry == &text) {
                    // Remove every earlier copy from the raw list and
                    // the file, then fall through to the normal save:
                    // the deduped list's last entry cannot equal the
                    // new text anymore, so the plain save path records
                    // it at the end, where GNU puts it.
                    Self::erase_duplicate_entries(capacity, state, &ledger, &text);
                }
            }
        }
        // niubash#570: the store's save (reedline's record filter) drops
        // an entry identical to the previous one, but GNU history is
        // positional — consecutive duplicates in one session, and the
        // same line typed in two sessions, are all kept (dual-same probe:
        // two sessions typing `echo SAMECMD` must leave two copies in the
        // file). When the new line equals the last one, bypass the
        // filter: append the
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
                    if let Ok(history) = HistoryStore::load_from_file(capacity, &path) {
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
            // appended/rebuilt): the store's drop stands, exactly the
            // pre-existing behavior for a session with no file to write.
            // Nothing is recorded: the line is in neither memory nor file.
            // (The save takes the item by value and its result is dropped,
            // so the original is cloned for it and returned as-is.)
            let _ = state.history.save(item.clone());
            Self::sync_state(state);
            return Ok(item);
        }
        let saved = state.history.save(item);
        let synced = Self::sync_state(state);
        if let Ok(mut ledger) = ledger.lock() {
            ledger.note_saved(text, synced);
        }
        Ok(saved)
    }

    /// niubash#646, HISTCONTROL=erasedups: remove every earlier entry
    /// equal to `text` from the raw list AND the backing file, before
    /// the new copy is saved at the end.
    ///
    /// GNU erases from the in-memory list only, but niu's live sync
    /// has already written those copies to the file — leaving them
    /// there would resurrect them at the next session's load. The
    /// rewrite goes through the same staging rebuild `replace_all`
    /// uses, but WITHOUT its ledger reset: pending lines of other
    /// texts stay pending (the staged file holds them verbatim, so the
    /// session-end accounting still matches), erased pending lines
    /// leave the pending set (their file copies are gone), and the
    /// ledger base shrinks by the erased copies that sat inside it.
    /// Copies inside the clear's hidden prefix are cleared lines the
    /// flush would remove anyway; erasing them early just shrinks the
    /// view hide by the same count.
    fn erase_duplicate_entries(
        capacity: usize,
        state: &mut HistoryState,
        ledger: &Arc<Mutex<SessionLedger>>,
        text: &str,
    ) {
        let raw_entries: Vec<String> = state
            .history
            .search(SearchQuery::all_that_contain_rev(String::new()))
            .map(|items| {
                items
                    .into_iter()
                    .rev()
                    .map(|item| item.command_line)
                    .collect::<Vec<String>>()
            })
            .unwrap_or_default();
        if !raw_entries.iter().any(|entry| entry == text) {
            return;
        }
        let (base_lines, hidden_prefix) = match ledger.lock() {
            Ok(ledger) => (ledger.base_lines, state.hidden_prefix),
            Err(_) => (0, state.hidden_prefix),
        };
        let erased_in_prefix = raw_entries
            .iter()
            .take(hidden_prefix.min(raw_entries.len()))
            .filter(|entry| *entry == text)
            .count();
        let kept_in_base = raw_entries
            .iter()
            .take(base_lines.min(raw_entries.len()))
            .filter(|entry| *entry != text)
            .count();
        let kept: Vec<String> = raw_entries
            .into_iter()
            .filter(|entry| entry != text)
            .collect();
        let staged_path = match state.path.clone() {
            Some(path) => path,
            None => std::env::temp_dir()
                .join(format!("niubash-history-stage-{}.tmp", std::process::id())),
        };
        let staged = write_entries_staged(&staged_path, &kept);
        let rebuilt = staged.and_then(|()| HistoryStore::load_from_file(capacity, &staged_path));
        match rebuilt {
            Ok(history) => {
                state.history = history;
                if let Some(path) = state.path.clone() {
                    state.signature = file_signature_lossy(&path, state);
                }
                if let Ok(mut ledger) = ledger.lock() {
                    ledger.pending.retain(|pending| pending.text != text);
                    ledger.base_lines = kept_in_base;
                    if erased_in_prefix > 0 {
                        ledger.note_view_prefix_shrank(erased_in_prefix);
                    }
                }
                if erased_in_prefix > 0 {
                    state.hidden_prefix = state.hidden_prefix.saturating_sub(erased_in_prefix);
                }
                Self::sync_view(state, ledger);
            }
            Err(error) => {
                // History is auxiliary state: a failed erase leaves
                // the duplicates in place (the pre-#646 behavior for
                // this line) instead of failing the save.
                warn_io_failure(state, "history erasedups rewrite failed", error);
            }
        }
    }
}

/// Encode one history line exactly as reedline stores it on disk (an
/// embedded newline becomes the literal four characters `<\n>`).
fn encode_history_line(text: &str) -> String {
    text.replace('\n', "<\\n>")
}

/// Decode one raw history-file line back to its entry text (the exact
/// inverse of [`encode_history_line`], matching reedline's load path).
fn decode_history_line(line: &str) -> String {
    line.replace("<\\n>", "\n")
}

/// Stream `entries` to `path` in reedline's on-disk encoding — the
/// staging writer `replace_all` uses (niubash#595), shared with the
/// erasedups rewrite (niubash#646). Byte shape: each entry's `\n`
/// segments joined by `<\n>`, terminated by `\n` (so `""` writes a bare
/// newline and `"\n"` writes `<\n>\n`).
fn write_entries_staged(path: &Path, entries: &[String]) -> io::Result<()> {
    use std::io::{BufWriter, Write as _};
    let file = std::fs::File::create(path)?;
    let mut writer = BufWriter::new(file);
    for entry in entries {
        for (i, segment) in entry.split('\n').enumerate() {
            if i > 0 {
                writer.write_all(b"<\\n>")?;
            }
            writer.write_all(segment.as_bytes())?;
        }
        writer.write_all(b"\n")?;
    }
    writer.flush()
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

/// The HISTCONTROL bits the record path consults (niubash#646),
/// mirroring bashhist.c check_history_control: values are
/// colon-separated tokens; `ignoreboth` is ignorespace + ignoredups;
/// unknown tokens are ignored, as are the values when the variable is
/// unset or empty.
#[derive(Clone, Copy, Default)]
struct HistControl {
    ignore_space: bool,
    ignore_dups: bool,
    erase_dups: bool,
}

impl HistControl {
    fn parse(value: &str) -> Self {
        let mut control = Self::default();
        for token in value.split(':') {
            match token {
                "ignorespace" => control.ignore_space = true,
                "ignoredups" => control.ignore_dups = true,
                "ignoreboth" => {
                    control.ignore_space = true;
                    control.ignore_dups = true;
                }
                "erasedups" => control.erase_dups = true,
                _ => {}
            }
        }
        control
    }
}

/// Mirror of bashhist.c history_should_ignore (niubash#646): the
/// colon-separated HISTIGNORE patterns are trimmed, empty ones are
/// skipped, `&` anywhere in a pattern is replaced by the previous
/// history entry's text, and the (substituted) pattern must match the
/// WHOLE line as a shell glob. The engine's own copy of this function
/// (rubash history.rs) is private to the engine's session list, so the
/// host carries the same semantics for the REPL record path.
fn history_should_ignore(line: &str, last_entry: &str, histignore: &str) -> bool {
    if histignore.is_empty() {
        return false;
    }
    for raw in histignore.split(':') {
        let pattern = raw.trim();
        if pattern.is_empty() {
            continue;
        }
        let substituted = pattern.replace('&', last_entry);
        let pattern_chars: Vec<char> = substituted.chars().collect();
        let line_chars: Vec<char> = line.chars().collect();
        if glob_full_match(&pattern_chars, &line_chars) {
            return true;
        }
    }
    false
}

/// Whole-string shell glob match (`*`, `?`, `[...]` classes with
/// ranges and `^`/`!` negation, `\` quoting the next character) — the
/// fnmatch semantics HISTIGNORE patterns use, without fnmatch's
/// pathname/period flags (bashhist.c passes 0 flags beyond the
/// extended-glob toggle, and history lines are not pathnames).
fn glob_full_match(pattern: &[char], text: &[char]) -> bool {
    glob_match_at(pattern, 0, text, 0)
}

fn glob_match_at(pattern: &[char], mut pi: usize, text: &[char], mut ti: usize) -> bool {
    while pi < pattern.len() {
        match pattern[pi] {
            '*' => {
                let mut star_end = pi;
                while star_end < pattern.len() && pattern[star_end] == '*' {
                    star_end += 1;
                }
                if star_end == pattern.len() {
                    return true;
                }
                for skip in ti..=text.len() {
                    if glob_match_at(pattern, star_end, text, skip) {
                        return true;
                    }
                }
                return false;
            }
            '?' => {
                if ti >= text.len() {
                    return false;
                }
                pi += 1;
                ti += 1;
            }
            '[' => {
                if ti >= text.len() {
                    return false;
                }
                match glob_class_match(pattern, pi, text[ti]) {
                    Some((true, next)) => {
                        pi = next;
                        ti += 1;
                    }
                    Some((false, _)) => return false,
                    None => {
                        // No closing bracket: a literal '['.
                        if text[ti] != '[' {
                            return false;
                        }
                        pi += 1;
                        ti += 1;
                    }
                }
            }
            '\\' if pi + 1 < pattern.len() => {
                if ti >= text.len() || text[ti] != pattern[pi + 1] {
                    return false;
                }
                pi += 2;
                ti += 1;
            }
            literal => {
                if ti >= text.len() || text[ti] != literal {
                    return false;
                }
                pi += 1;
                ti += 1;
            }
        }
    }
    ti == text.len()
}

/// Match one character against a `[...]` class starting at
/// `pattern[start]`. Returns `(matched, index just past ']'`), or
/// `None` when the class is unterminated (the `[` is literal then).
fn glob_class_match(pattern: &[char], start: usize, c: char) -> Option<(bool, usize)> {
    let mut i = start + 1;
    let negated = i < pattern.len() && (pattern[i] == '^' || pattern[i] == '!');
    if negated {
        i += 1;
    }
    let mut matched = false;
    let mut first = true;
    while i < pattern.len() && (pattern[i] != ']' || first) {
        first = false;
        if i + 2 < pattern.len() && pattern[i + 1] == '-' && pattern[i + 2] != ']' {
            if pattern[i] <= c && c <= pattern[i + 2] {
                matched = true;
            }
            i += 3;
        } else {
            if pattern[i] == c {
                matched = true;
            }
            i += 1;
        }
    }
    if i >= pattern.len() {
        return None;
    }
    Some((matched != negated, i + 1))
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
    /// niubash#642: base lines — already in the file when this session
    /// started — that a `history -c` in this session cleared, as plain
    /// entry texts. `cleared` alone cannot cover them: they were never
    /// this session's pending lines, so without this set the
    /// session-end write left them in the file and the next session
    /// loaded lines the user had cleared ("already cleared lines come
    /// back"). GNU's exit write leaves them too (bashhist.c
    /// maybe_save_shell_history only appends session lines); #642 rules
    /// the stronger contract for niubash — what the user cleared stays
    /// cleared. Removal at flush time is confined to the file's first
    /// `base_lines` lines, the window the base occupied at startup, so
    /// a foreign line appended later is never mistaken for a base line
    /// even when it shares the cleared text.
    ///
    /// Replaced wholesale on every clear (a later clear's file snapshot
    /// is the authority; the file does not change mid-session, so the
    /// base window it captures is the same one). Dropped by an explicit
    /// re-read (the lines are loaded back into the list on purpose) and
    /// by an authoritative rewrite, and consumed once the flush has
    /// applied it.
    cleared_base: Vec<String>,
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
            cleared_base: Vec::new(),
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
    fn note_cleared(&mut self, view_len: usize, file_lines_now: &[String]) {
        self.cleared.extend(self.pending.drain(..).map(|p| p.text));
        // niubash#642: the base lines this clear retires are the file
        // snapshot's first `base_lines` entries — the window the base
        // occupied at startup. Lines another shell appended after
        // startup sit past the window and are never this session's to
        // remove. Replace, don't extend: the file is unchanged since
        // any earlier clear in this session, so its snapshot captures
        // the same base window again (extending would double-count a
        // text and could eat a foreign same-text line at flush time).
        let base_window = self.base_lines.min(file_lines_now.len());
        self.cleared_base = file_lines_now[..base_window].to_vec();
        self.view_hidden_len = view_len;
        self.view_gen += 1;
    }

    /// niubash#646 (erasedups): `removed` entries were physically
    /// removed from the raw list's hidden prefix (they were cleared
    /// lines the erase rewrote out of the file early), so the view
    /// hide shrinks by that many instead of masking live entries.
    fn note_view_prefix_shrank(&mut self, removed: usize) {
        self.view_hidden_len = self.view_hidden_len.saturating_sub(removed);
    }

    /// An explicit re-read (`history -r`/`-n`) loads file lines back
    /// into the list on purpose — the bash-it `-c && -r` cycle depends
    /// on it (niubash#182) — so the clear's view hide ends here.
    fn note_view_reloaded(&mut self) {
        self.view_hidden_len = 0;
        // niubash#642: the re-read loads the cleared base lines back
        // into the list on purpose (the bash-it `-a && -c && -r`
        // fixed point depends on it), so the session-end write must
        // not remove them after all.
        self.cleared_base.clear();
        self.view_gen += 1;
    }

    /// Authoritative rewrite (-w to the backing file, replace): the file
    /// now holds exactly what memory holds, so both sets reset and the
    /// base moves to the rewritten file's line count.
    fn reset(&mut self, base_lines: usize) {
        self.pending.clear();
        self.cleared.clear();
        self.cleared_base.clear();
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
            self.cleared_base.clear();
        }
        self.base_lines = base_lines;
    }
}

impl History for LiveFileBackedHistory {
    fn save(&mut self, item: HistoryItem) -> Result<HistoryItem> {
        self.save_entry(item, true)
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
        let (hidden, _) = Self::view_bounds(&state);
        if hidden == 0 {
            return state.history.count(query);
        }
        Ok(Self::view_search(&state, query, None)?.len() as i64)
    }

    fn search(&self, query: SearchQuery) -> Result<Vec<HistoryItem>> {
        let capacity = self.capacity;
        let mut state = self.lock()?;
        Self::refresh_if_stale(capacity, &mut state);
        Self::sync_view(&mut state, &self.ledger);
        let (hidden, _) = Self::view_bounds(&state);
        let not_command_line = Self::cursor_not_command_line(&state, &query);
        if hidden == 0 {
            return state
                .history
                .search_filtered(query, not_command_line.as_deref());
        }
        Self::view_search(&state, query, not_command_line.as_deref())
    }

    fn update(
        &mut self,
        id: HistoryItemId,
        updater: &dyn Fn(HistoryItem) -> HistoryItem,
    ) -> Result<()> {
        let capacity = self.capacity;
        let state = self.lock_mut()?;
        Self::refresh_if_stale(capacity, state);
        // The store keeps reedline's answer verbatim: file-backed
        // histories do not support update.
        let _ = (id, updater);
        state.history.update()
    }

    fn clear(&mut self) -> Result<()> {
        let capacity = self.capacity;
        let state = self.lock_mut()?;
        Self::refresh_if_stale(capacity, state);
        // Reedline's clear emptied the list AND removed the backing
        // file; the store only owns the list, so the removal is spelled
        // out here, in the same order (entries first, then the file).
        state.history.clear_entries();
        if let Some(path) = state.path.clone() {
            std::fs::remove_file(path)?;
        }
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
        // The store keeps reedline's answer verbatim: file-backed
        // histories do not support delete.
        let _ = id;
        state.history.delete()
    }

    fn sync(&mut self) -> io::Result<()> {
        let state = self.state.get_mut().map_err(|_| history_lock_error())?;
        Self::sync_state(state);
        Ok(())
    }

    fn session(&self) -> Option<HistorySessionId> {
        // Reedline's file-backed history never carries a session id
        // (its `session` is a constant `None`); the store keeps that.
        None
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
    fn cursor_shaped_search_skips_entries_equal_to_the_shown_one() {
        // reedline's HistoryCursor (skip_dupes hardcoded on) navigates
        // with limit 1 and start_id = the entry it is showing, carrying
        // its skip value in the filter's `not_command_line` field —
        // `pub(crate)` in reedline, so outside code can neither set nor
        // read it (this test's query physically carries None). The
        // wrapper rebuilds the exclusion from that closed shape:
        // stepping back from the second "echo dup" must land on
        // "echo other", not on the first "echo dup".
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("history");
        let mut history = LiveFileBackedHistory::with_file(100, path);
        for line in ["echo other", "echo dup", "echo dup"] {
            history.save(HistoryItem::from_command_line(line)).unwrap();
        }
        let query = SearchQuery {
            direction: SearchDirection::Backward,
            start_time: None,
            end_time: None,
            start_id: Some(HistoryItemId::new(2)),
            end_id: None,
            limit: Some(1),
            filter: reedline::SearchFilter::anything(None),
        };
        let found = history.search(query).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].command_line, "echo other");
        assert_eq!(found[0].id, Some(HistoryItemId::new(0)));
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
    fn issue595_replace_all_streams_byte_identical_staging() {
        // niubash#595: `replace_all` used to build the whole staged file
        // as one giant String before writing it. The streaming writer
        // must produce byte-identical staging, including the awkward
        // shapes: embedded newlines escape to `<\n>`, empty entries are
        // bare newlines, and consecutive duplicates survive (the
        // reedline load path decodes `<\n>` back on read).
        use rubash::history::HistoryProvider as _;
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("history");
        let mut provider = RubashHistoryProvider::with_file(100, path.clone());
        provider
            .replace(vec![
                "plain".to_string(),
                "multi\nline".to_string(),
                "".to_string(),
                "dup".to_string(),
                "dup".to_string(),
                "\n".to_string(),
            ])
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "plain\nmulti<\\n>line\n\ndup\ndup\n<\\n>\n"
        );
        assert_eq!(
            provider.entries().unwrap(),
            ["plain", "multi\nline", "", "dup", "dup", "\n"]
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
    fn issue645_read_new_history_positional_delivery() {
        // niubash#645 (was read_new_history_does_not_duplicate_lines_
        // from_another_session): the old pin framed `history -n` as a
        // TEXT-SET dedupe — a line whose text is already known is not
        // delivered again. GNU's -n is POSITIONAL (histfile.c
        // read_history_range appends the file lines past the reader's
        // position): a second session appending a line whose text this
        // session already holds delivers a SECOND copy, and -n itself
        // adds nothing beyond what the file holds. In the REPL
        // composition the live refresh has usually delivered the line
        // before -n runs; the pin asserts exactly that end state.
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("history");
        std::fs::write(&path, "seedcmd\n").unwrap();

        let mut first = RubashHistoryProvider::with_file(100, path.clone());
        let mut second = RubashHistoryProvider::with_file(100, path.clone());

        // Phase 1 (the niubash#571 value, kept): a different-text line
        // from the other session is picked up once, and the file gains
        // no duplicate from the pickup itself.
        second.append("ls".to_string()).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "seedcmd\nls\n");
        first.read_new_history(path.to_str().unwrap()).unwrap();
        assert_eq!(first.entries().unwrap(), vec!["seedcmd", "ls"]);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "seedcmd\nls\n",
            "-n must not duplicate the line it just picked up"
        );

        // Phase 2 (the positional semantics the old pin denied): the
        // other session appends the SAME text again. The line count
        // for that text goes 1 -> 2 in the list — a text-set dedupe
        // would have kept it at 1 — and a further -n changes nothing.
        second.append("ls".to_string()).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "seedcmd\nls\nls\n");
        assert_eq!(
            first.entries().unwrap(),
            vec!["seedcmd", "ls", "ls"],
            "a same-text line from another session is a second entry, not a dupe"
        );
        first.read_new_history(path.to_str().unwrap()).unwrap();
        assert_eq!(first.entries().unwrap(), vec!["seedcmd", "ls", "ls"]);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "seedcmd\nls\nls\n",
            "-n adds nothing beyond what the file holds"
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
            "from-another-shell\n",
            "the cleared base line is removed (niubash#642); the line \
             another shell appended survives the session-end write"
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
            "KEPT\n",
            "cleared lines must not survive the session-end write — \
             the seeds this session cleared included (niubash#642)"
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
            "COMMITTED\n",
            "-a-committed lines survive -c and the session-end write; \
             the cleared seeds do not (niubash#642)"
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
            "",
            "every cleared line is gone — the session's own line and \
             the seeds it loaded (niubash#642)"
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
            "",
            "the twin's -c removes the line the editor recorded and \
             the cleared seed (niubash#642)"
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
            "history\necho AFTER\n",
            "the session-end write removes exactly the cleared lines, \
             seeds included (niubash#642)"
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

    /// A provider whose shared settings cell is pre-loaded the way
    /// `Shell::refresh_history_settings` would leave it (niubash#646).
    fn provider_with_settings(
        path: &std::path::Path,
        configure: impl FnOnce(&mut HistorySettings),
    ) -> RubashHistoryProvider {
        let provider = RubashHistoryProvider::with_file(100, path.to_path_buf());
        let settings = provider.settings();
        let mut settings = settings.lock().unwrap();
        configure(&mut settings);
        drop(settings);
        provider
    }

    #[test]
    fn issue642_dual_session_interleave_clear_reconciles() {
        // niubash#642, dual-session interleave, checked against the
        // four contamination criteria: A starts over two seeds, both
        // sessions append, A clears, B appends more, A appends and
        // ends. A's session-end write must remove the seeds A cleared
        // (no resurrection of cleared lines) and A's own cleared line,
        // while every line B appended — before AND after A's clear —
        // survives (no loss), nothing is written twice (no
        // duplication), and no lines merge.
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("history");
        std::fs::write(&path, "seedA\nseedB\n").unwrap();

        let mut a = RubashHistoryProvider::with_file(100, path.clone());
        let mut b = RubashHistoryProvider::with_file(100, path.clone());
        a.append("A1".to_string()).unwrap();
        b.append("B1".to_string()).unwrap();
        a.clear().unwrap();
        b.append("B2".to_string()).unwrap();
        a.append("A2".to_string()).unwrap();

        drop(a);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "B1\nB2\nA2\n",
            "cleared seeds and A's cleared line are gone; B's lines all survive"
        );
        // B's own session end changes nothing further.
        drop(b);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "B1\nB2\nA2\n");
    }

    #[test]
    fn issue642_clear_then_reread_keeps_base_lines() {
        // niubash#642 x niubash#182: an explicit re-read after -c
        // restores the cleared lines on purpose — the session-end
        // write must then keep them (the bash-it `-a && -c && -r`
        // fixed point).
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("history");
        std::fs::write(&path, "seedA\nseedB\n").unwrap();

        let mut provider = RubashHistoryProvider::with_file(100, path.clone());
        provider.clear().unwrap();
        provider.read_history(path.to_str().unwrap()).unwrap();
        assert_eq!(provider.entries().unwrap(), vec!["seedA", "seedB"]);
        drop(provider);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "seedA\nseedB\n");
    }

    #[test]
    fn issue646_histcontrol_ignoreboth_filters_recording() {
        // niubash#646: HISTCONTROL=ignoreboth on the REPL record path —
        // a duplicate of the previous entry and a space-prefixed line
        // are never recorded (GNU bashhist.c check_history_control),
        // in the list or in the file.
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("history");
        let mut provider = provider_with_settings(&path, |settings| {
            settings.histcontrol = "ignoreboth".to_string();
        });
        provider.append("echo one".to_string()).unwrap();
        provider.append("echo one".to_string()).unwrap();
        provider.append(" secretcmd".to_string()).unwrap();
        provider.append("echo two".to_string()).unwrap();
        assert_eq!(provider.entries().unwrap(), vec!["echo one", "echo two"]);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "echo one\necho two\n"
        );
    }

    #[test]
    fn issue646_histignore_patterns_filter_recording() {
        // niubash#646: HISTIGNORE filters at record time (GNU
        // bashhist.c history_should_ignore): `secret*` matches the
        // whole line as a glob, and `&` stands for the previous entry —
        // the second `echo start` is ignored because the previous
        // RECORDED entry is `echo start` (the filtered secret line
        // never became an entry).
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("history");
        let mut provider = provider_with_settings(&path, |settings| {
            settings.histignore = "secret*:&".to_string();
        });
        provider.append("echo start".to_string()).unwrap();
        provider.append("secretstuff".to_string()).unwrap();
        provider.append("echo start".to_string()).unwrap();
        provider.append("echo other".to_string()).unwrap();
        assert_eq!(
            provider.entries().unwrap(),
            vec!["echo start", "echo other"]
        );
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "echo start\necho other\n"
        );
    }

    #[test]
    fn issue646_histcontrol_erasedups_removes_earlier_copies() {
        // niubash#646: HISTCONTROL=erasedups — recording `a` again
        // removes the earlier copy from the list AND the file (the
        // live sync had already written it there), and the new copy
        // lands at the end, where GNU puts it.
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("history");
        let mut provider = provider_with_settings(&path, |settings| {
            settings.histcontrol = "erasedups".to_string();
        });
        provider.append("a".to_string()).unwrap();
        provider.append("b".to_string()).unwrap();
        provider.append("a".to_string()).unwrap();
        assert_eq!(provider.entries().unwrap(), vec!["b", "a"]);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "b\na\n");
    }

    #[test]
    fn issue646_histsize_stifles_the_list_view_only() {
        // niubash#646: HISTSIZE=3 — the list face shows only the last
        // three entries, while the file keeps everything (GNU's exit
        // append leaves the file's older lines in place the same way).
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("history");
        let mut provider = provider_with_settings(&path, |settings| {
            settings.histsize = Some(3);
        });
        for entry in ["e1", "e2", "e3", "e4", "e5"] {
            provider.append(entry.to_string()).unwrap();
        }
        assert_eq!(provider.entries().unwrap(), vec!["e3", "e4", "e5"]);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "e1\ne2\ne3\ne4\ne5\n",
            "the stifle is a view, not a file truncation"
        );
    }

    #[test]
    fn issue646_histfilesize_truncates_at_session_end() {
        // niubash#646: HISTFILESIZE=4 — the session-end write leaves
        // the file holding only its last four lines (bashhist.c
        // maybe_save_shell_history ends with the HISTFILESIZE stifle).
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("history");
        std::fs::write(&path, "seed0\nseed1\nseed2\nseed3\nseed4\nseed5\n").unwrap();
        let mut provider = provider_with_settings(&path, |settings| {
            settings.histfilesize = Some(4);
        });
        provider.append("n1".to_string()).unwrap();
        provider.append("n2".to_string()).unwrap();
        drop(provider);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "seed4\nseed5\nn1\nn2\n"
        );
    }

    #[test]
    fn issue646_histappend_keeps_cleared_base_lines_at_session_end() {
        // niubash#646: with histappend on, the session-end write takes
        // GNU's append branch (bashhist.c maybe_save_shell_history +
        // bash_clear_history zeroing the session counter): the cleared
        // BASE lines stay in the file — #642's base removal belongs to
        // the default mode — while the session's own cleared line is
        // still removed (GNU never wrote it in the first place; niu's
        // live sync did, so the flush cancels exactly that artifact,
        // the niubash#569 grid).
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("history");
        std::fs::write(&path, "seedA\nseedB\n").unwrap();
        let mut provider = provider_with_settings(&path, |settings| {
            settings.histappend = true;
        });
        provider.append("OWN".to_string()).unwrap();
        provider.clear().unwrap();
        provider.append("KEPT".to_string()).unwrap();
        drop(provider);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "seedA\nseedB\nKEPT\n"
        );
    }

    #[test]
    fn issue646_histfilesize_truncates_write_history() {
        // niubash#646: `history -w` writes at most HISTFILESIZE lines
        // (histfile.c history_write). niu's reader is a live mirror of
        // the file (niubash#572), so the list face follows the
        // truncated file at the next query — where GNU's independent
        // in-memory list would still show w1 until the session ends.
        // That divergence is the mirror architecture's, not the
        // truncation's; the session-end face (the main HISTFILESIZE
        // consumer) is unaffected because the session is over.
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("history");
        let mut provider = provider_with_settings(&path, |settings| {
            settings.histfilesize = Some(2);
        });
        provider.append("w1".to_string()).unwrap();
        provider.append("w2".to_string()).unwrap();
        provider.append("w3".to_string()).unwrap();
        provider.write_history(path.to_str().unwrap()).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "w2\nw3\n");
        assert_eq!(provider.entries().unwrap(), vec!["w2", "w3"]);
    }

    #[test]
    fn issue660_editor_saves_drop_lines_while_history_option_off() {
        // niubash#660: GNU records a typed line only while the
        // `history` shell option is on (bashhist.c
        // remember_on_history gates maybe_add_history). The editor
        // instance carries the gate; the provider does not — GNU's
        // `history -s` pushes its operand through add_history even
        // while the option is off (verified against bash 5.3.20: the
        // -s entry lands in the list and the file, the typed lines
        // around it do not).
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("history");
        let mut provider = RubashHistoryProvider::with_file(100, path.clone());
        // The production sharing: the editor twin is built over the
        // provider's ledger and settings cells (build_line_editor).
        let mut editor = LiveFileBackedHistory::with_shared_ledger_and_settings(
            100,
            path.clone(),
            provider.ledger(),
            provider.settings(),
        );
        editor.set_history_option_gate();
        provider.settings().lock().unwrap().history_enabled = false;

        use reedline::History as _;
        editor
            .save(HistoryItem::from_command_line("secret-while-off"))
            .unwrap();
        assert!(
            !path.exists() || std::fs::read_to_string(&path).unwrap().is_empty(),
            "a line typed while the option is off must not reach the file"
        );
        assert!(provider.entries().unwrap().is_empty());

        // `history -s` (provider append) is not gated.
        provider.append("s-added-while-off".to_string()).unwrap();
        assert_eq!(provider.entries().unwrap(), vec!["s-added-while-off"]);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "s-added-while-off\n"
        );

        // The option back on: the editor records again.
        provider.settings().lock().unwrap().history_enabled = true;
        editor
            .save(HistoryItem::from_command_line("visible-after-on"))
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "s-added-while-off\nvisible-after-on\n"
        );
    }

    #[test]
    fn issue661_retarget_moves_backing_and_later_saves_follow() {
        // niubash#661: GNU reads HISTFILE at the moment of each
        // operation, so a mid-session redirect moves the stream: the
        // new file takes the session's list, later saves and the
        // session-end write land there, and the old file gains
        // nothing after the switch.
        let temp = tempfile::tempdir().unwrap();
        let a = temp.path().join("histA");
        let b = temp.path().join("histB");
        std::fs::write(&a, "seedA\n").unwrap();
        let mut provider = RubashHistoryProvider::with_file(100, a.clone());
        provider.append("session-line".to_string()).unwrap();
        assert_eq!(
            std::fs::read_to_string(&a).unwrap(),
            "seedA\nsession-line\n"
        );

        provider.retarget(b.clone());
        assert_eq!(provider.backing_file(), Some(b.clone()));
        assert_eq!(
            std::fs::read_to_string(&b).unwrap(),
            "seedA\nsession-line\n",
            "the new file holds the session list at switch time"
        );

        provider.append("after-redirect".to_string()).unwrap();
        assert_eq!(
            std::fs::read_to_string(&b).unwrap(),
            "seedA\nsession-line\nafter-redirect\n"
        );
        assert_eq!(
            std::fs::read_to_string(&a).unwrap(),
            "seedA\nsession-line\n",
            "the old file gains nothing after the switch"
        );

        // The session-end write (provider Drop) settles in B too.
        drop(provider);
        assert_eq!(
            std::fs::read_to_string(&b).unwrap(),
            "seedA\nsession-line\nafter-redirect\n"
        );
        assert_eq!(
            std::fs::read_to_string(&a).unwrap(),
            "seedA\nsession-line\n"
        );
    }
}
