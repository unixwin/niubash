//! Live, file-backed history for the interactive Reedline session.

use std::{
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
    sync::{Mutex, MutexGuard},
    time::SystemTime,
};

use reedline::{
    FileBackedHistory, History, HistoryItem, HistoryItemId, HistorySessionId, ReedlineError,
    Result, SearchQuery,
};

use crate::config::HistoryMode;

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
    pub(crate) fn with_file(capacity: usize, path: PathBuf, mode: HistoryMode) -> Self {
        Self {
            inner: LiveFileBackedHistory::with_mode(capacity, path, mode),
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
        // Full rewrite (`history -d`, the interactive recording veto):
        // memory AND file end up holding exactly `entries`. Unlike -c this
        // deliberately truncates the file, so it uses the truncating clear.
        self.inner
            .clear()
            .map_err(|error| io::Error::other(error.to_string()))?;
        for entry in entries {
            self.append(entry)?;
        }
        Ok(())
    }

    fn write_history(&mut self, path: &str) -> io::Result<()> {
        ensure_history_file_accessible(std::path::Path::new(path));
        let entries = self.entries()?;
        if entries.is_empty() {
            std::fs::write(path, "")?;
        } else {
            std::fs::write(path, entries.join("\n") + "\n")?;
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
        let entries = self.entries()?;
        if entries.is_empty() {
            return Ok(());
        }
        // builtins/history.def -a appends only what the file lacks
        // (bashhist.c appends history_lines_this_session). The live history
        // syncs every save to the file, so in the common case every entry is
        // already there and `-a` is a no-op — and that is exactly what breaks
        // the bash-it theme interlock (niubash#182): codeword/gitline run
        // `history -a && history -c && history -r` from PROMPT_COMMAND on
        // every prompt, and re-appending the whole list doubled the file each
        // cycle, which `history -r` then loaded back into memory — a
        // self-amplifying loop that wedged the session.
        let existing: std::collections::HashSet<String> = std::fs::read_to_string(path)
            .unwrap_or_default()
            .lines()
            .map(|l| l.trim().to_string())
            .collect();
        let new_entries: Vec<&String> = entries
            .iter()
            .filter(|entry| !existing.contains(entry.trim()))
            .collect();
        if new_entries.is_empty() {
            return Ok(());
        }
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?;
        for entry in new_entries {
            writeln!(file, "{}", entry)?;
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
        // Fast path (niubash#182): append the missing lines to the backing
        // file directly, then rebuild the reader from disk. Routing them
        // through per-entry saves made reedline re-append its whole unwritten
        // buffer (the file's own content) on every save.
        let requested = std::path::Path::new(path);
        if self
            .inner
            .backing_file()
            .is_some_and(|backing| backing == requested)
        {
            use std::io::Write as _;
            let mut file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)?;
            for line in &missing {
                writeln!(file, "{line}")?;
            }
            drop(file);
            if self.inner.reload_from_file().is_ok() {
                return Ok(());
            }
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
    mode: HistoryMode,
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
    #[cfg(test)]
    pub(crate) fn with_file(capacity: usize, path: PathBuf) -> Self {
        Self::with_mode(capacity, path, HistoryMode::Shared)
    }

    /// Build the live history. Infallible: a history file that cannot be
    /// opened degrades to an in-memory history (see the struct docs for the
    /// GNU citation). A file that opens but cannot be stat-ed (restricted
    /// token denying `metadata` while `CreateFileW` succeeded) degrades the
    /// same way instead of aborting shell startup with `os error 5`.
    pub(crate) fn with_mode(capacity: usize, path: PathBuf, mode: HistoryMode) -> Self {
        // On Windows, ensure the history file has accessible ACLs before opening.
        // Codex sandbox and similar environments may create files with restrictive
        // permissions that block read access (os error 5).
        ensure_history_file_accessible(&path);

        let history_capacity = if mode == HistoryMode::Private {
            usize::MAX - 1
        } else {
            capacity
        };
        let state = match FileBackedHistory::with_file(history_capacity, path.clone()) {
            Ok(history) => {
                let mut state = HistoryState {
                    history,
                    signature: FileSignature::default(),
                    path: Some(path.clone()),
                    warned_io_failure: false,
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
                    history: in_memory_history(history_capacity),
                    signature: FileSignature::default(),
                    path: None,
                    warned_io_failure: true,
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
            mode,
        }
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

    fn refresh_if_stale(capacity: usize, state: &mut HistoryState, mode: HistoryMode) {
        let Some(path) = state.path.clone() else {
            return;
        };
        if mode != HistoryMode::Shared {
            return;
        }

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

    fn sync_state(state: &mut HistoryState) -> io::Result<()> {
        if let Err(error) = state.history.sync() {
            // GNU bash never fails a command because the history file
            // cannot be written: append/exit writes report nothing
            // (bashhist.c), and only the explicit `history -w`/`-a`
            // builtins surface I/O errors. Keep the entries in memory,
            // warn once, and let later reads still see them.
            warn_io_failure(state, "history file sync failed", error);
        }
        let path = state.path.clone();
        state.signature = match path.as_deref() {
            Some(path) => file_signature_lossy(path, state),
            None => FileSignature::default(),
        };
        Ok(())
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
        let capacity = self.capacity;
        let mut state = self.lock_mut()?;
        let Some(path) = state.path.clone() else {
            return Err(ReedlineError::from(history_lock_error()));
        };
        match FileBackedHistory::with_file(capacity, path.clone()) {
            Ok(history) => {
                state.history = history;
                state.signature = file_signature_lossy(&path, &mut state);
                Ok(())
            }
            Err(error) => Err(ReedlineError::from(io::Error::other(error.to_string()))),
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
        let mode = self.mode;
        let mut state = self.lock_mut()?;
        Self::refresh_if_stale(capacity, &mut state, mode);
        let snapshot = state
            .path
            .as_deref()
            .and_then(|path| std::fs::read(path).ok());
        state.history.clear()?;
        match (state.path.clone(), snapshot) {
            (Some(path), Some(bytes)) => {
                if std::fs::write(&path, &bytes).is_ok() {
                    state.signature = file_signature_lossy(&path, &mut state);
                    return Ok(());
                }
                // Could not restore: degrade the signature so the next
                // query reloads whatever is on disk.
                state.signature = file_signature_lossy(&path, &mut state);
                Ok(())
            }
            (path, _) => {
                state.signature = match path.as_deref() {
                    Some(path) => file_signature_lossy(path, &mut state),
                    None => FileSignature::default(),
                };
                Ok(())
            }
        }
    }

    /// The file this history is backed by, when it has one.
    pub(crate) fn backing_file(&self) -> Option<PathBuf> {
        self.state.lock().ok().and_then(|state| state.path.clone())
    }
}

impl History for LiveFileBackedHistory {
    fn save(&mut self, item: HistoryItem) -> Result<HistoryItem> {
        let capacity = self.capacity;
        let mode = self.mode;
        let mut state = self.lock_mut()?;
        Self::refresh_if_stale(capacity, &mut state, mode);
        let saved = state.history.save(item)?;
        Self::sync_state(&mut state)?;
        Ok(saved)
    }

    fn load(&self, id: HistoryItemId) -> Result<HistoryItem> {
        let capacity = self.capacity;
        let mode = self.mode;
        let mut state = self.lock()?;
        Self::refresh_if_stale(capacity, &mut state, mode);
        state.history.load(id)
    }

    fn count(&self, query: SearchQuery) -> Result<i64> {
        let capacity = self.capacity;
        let mode = self.mode;
        let mut state = self.lock()?;
        Self::refresh_if_stale(capacity, &mut state, mode);
        state.history.count(query)
    }

    fn search(&self, query: SearchQuery) -> Result<Vec<HistoryItem>> {
        let capacity = self.capacity;
        let mode = self.mode;
        let mut state = self.lock()?;
        Self::refresh_if_stale(capacity, &mut state, mode);
        state.history.search(query)
    }

    fn update(
        &mut self,
        id: HistoryItemId,
        updater: &dyn Fn(HistoryItem) -> HistoryItem,
    ) -> Result<()> {
        let capacity = self.capacity;
        let mode = self.mode;
        let mut state = self.lock_mut()?;
        Self::refresh_if_stale(capacity, &mut state, mode);
        state.history.update(id, updater)
    }

    fn clear(&mut self) -> Result<()> {
        let capacity = self.capacity;
        let mode = self.mode;
        let mut state = self.lock_mut()?;
        Self::refresh_if_stale(capacity, &mut state, mode);
        state.history.clear()?;
        let path = state.path.clone();
        state.signature = match path.as_deref() {
            Some(path) => file_signature_lossy(path, &mut state),
            None => FileSignature::default(),
        };
        Ok(())
    }

    fn delete(&mut self, id: HistoryItemId) -> Result<()> {
        let capacity = self.capacity;
        let mode = self.mode;
        let mut state = self.lock_mut()?;
        Self::refresh_if_stale(capacity, &mut state, mode);
        state.history.delete(id)
    }

    fn sync(&mut self) -> io::Result<()> {
        let state = self.state.get_mut().map_err(|_| history_lock_error())?;
        Self::sync_state(state)
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
    io::Error::new(io::ErrorKind::Other, HISTORY_LOCK_ERROR)
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

        let mut history = LiveFileBackedHistory::with_mode(100, path, HistoryMode::Shared);
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

        let mut history = LiveFileBackedHistory::with_mode(100, path.clone(), HistoryMode::Shared);
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
        let mut provider = RubashHistoryProvider::with_file(
            100,
            parent.join(".niubash_history"),
            HistoryMode::Shared,
        );
        let entries = provider.entries().unwrap();
        assert!(entries.is_empty());
        provider.append("memory only".to_string()).unwrap();
        assert_eq!(provider.entries().unwrap(), vec!["memory only"]);
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
    fn private_mode_loads_existing_history_but_ignores_later_external_writes() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("history");
        std::fs::write(&path, "old-one\nold-two\n").unwrap();

        let mut first = LiveFileBackedHistory::with_mode(100, path, HistoryMode::Private);
        first
            .save(HistoryItem::from_command_line("this-session"))
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
        assert_eq!(commands, vec!["old-one", "old-two", "this-session"]);
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

        let mut provider = RubashHistoryProvider::with_file(100, path.clone(), HistoryMode::Shared);
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

        let mut provider = RubashHistoryProvider::with_file(100, path.clone(), HistoryMode::Shared);
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

        let mut provider = RubashHistoryProvider::with_file(100, path.clone(), HistoryMode::Shared);
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
    fn clear_leaves_the_file_and_later_append_adds_only_new_entries() {
        // GNU `history -c` clears memory only; the file keeps its lines.
        // A later `history -a` appends exactly the new session entry.
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("history");

        let mut provider = RubashHistoryProvider::with_file(100, path.clone(), HistoryMode::Shared);
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

        let mut provider = RubashHistoryProvider::with_file(100, path.clone(), HistoryMode::Shared);
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
}
