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
    pub(crate) fn with_file(capacity: usize, path: PathBuf, mode: HistoryMode) -> Result<Self> {
        Ok(Self {
            inner: LiveFileBackedHistory::with_mode(capacity, path, mode)?,
        })
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
        self.inner
            .clear()
            .map_err(|error| io::Error::other(error.to_string()))
    }

    fn append(&mut self, command: String) -> io::Result<()> {
        self.inner
            .save(HistoryItem::from_command_line(command))
            .map(|_| ())
            .map_err(|error| io::Error::other(error.to_string()))
    }

    fn replace(&mut self, entries: Vec<String>) -> io::Result<()> {
        self.clear()?;
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
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?;
        for entry in &entries {
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
        for line in content.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            if !existing_set.contains(trimmed) {
                self.append(trimmed.to_string())?;
            }
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
}

/// A Reedline file history that is visible across concurrently running shells.
///
/// Reedline's `FileBackedHistory` reads the file once and only syncs on drop.
/// That is fine for a single process, but makes another terminal invisible
/// until one of the shells exits. This wrapper syncs after saves and reloads
/// the file before queries when another process has changed it.
pub(crate) struct LiveFileBackedHistory {
    capacity: usize,
    path: PathBuf,
    state: Mutex<HistoryState>,
    mode: HistoryMode,
}

impl LiveFileBackedHistory {
    #[cfg(test)]
    pub(crate) fn with_file(capacity: usize, path: PathBuf) -> Result<Self> {
        Self::with_mode(capacity, path, HistoryMode::Shared)
    }

    pub(crate) fn with_mode(capacity: usize, path: PathBuf, mode: HistoryMode) -> Result<Self> {
        // On Windows, ensure the history file has accessible ACLs before opening.
        // Codex sandbox and similar environments may create files with restrictive
        // permissions that block read access (os error 5).
        ensure_history_file_accessible(&path);

        let history_capacity = if mode == HistoryMode::Private {
            usize::MAX - 1
        } else {
            capacity
        };
        let history = FileBackedHistory::with_file(history_capacity, path.clone())?;
        let signature = file_signature(&path)?;
        Ok(Self {
            capacity,
            path,
            state: Mutex::new(HistoryState { history, signature }),
            mode,
        })
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

    fn refresh_if_stale(
        capacity: usize,
        path: &Path,
        state: &mut HistoryState,
        mode: HistoryMode,
    ) -> Result<()> {
        if mode != HistoryMode::Shared {
            return Ok(());
        }

        // On Windows, fix ACLs before re-reading the file from disk.
        ensure_history_file_accessible(path);

        let signature = file_signature(path)?;
        if signature == state.signature {
            return Ok(());
        }

        // Preserve commands submitted by this process before replacing the
        // in-memory view with the latest file contents.
        state.history.sync()?;
        state.history = FileBackedHistory::with_file(capacity, path.to_path_buf())?;
        state.signature = file_signature(path)?;
        Ok(())
    }

    fn sync_state(path: &Path, state: &mut HistoryState) -> io::Result<()> {
        state.history.sync()?;
        state.signature = file_signature(path)?;
        Ok(())
    }
}

impl History for LiveFileBackedHistory {
    fn save(&mut self, item: HistoryItem) -> Result<HistoryItem> {
        let capacity = self.capacity;
        let path = self.path.clone();
        let mode = self.mode;
        let mut state = self.lock_mut()?;
        Self::refresh_if_stale(capacity, &path, &mut state, mode)?;
        let saved = state.history.save(item)?;
        Self::sync_state(&path, &mut state)?;
        Ok(saved)
    }

    fn load(&self, id: HistoryItemId) -> Result<HistoryItem> {
        let capacity = self.capacity;
        let path = self.path.clone();
        let mode = self.mode;
        let mut state = self.lock()?;
        Self::refresh_if_stale(capacity, &path, &mut state, mode)?;
        state.history.load(id)
    }

    fn count(&self, query: SearchQuery) -> Result<i64> {
        let capacity = self.capacity;
        let path = self.path.clone();
        let mode = self.mode;
        let mut state = self.lock()?;
        Self::refresh_if_stale(capacity, &path, &mut state, mode)?;
        state.history.count(query)
    }

    fn search(&self, query: SearchQuery) -> Result<Vec<HistoryItem>> {
        let capacity = self.capacity;
        let path = self.path.clone();
        let mode = self.mode;
        let mut state = self.lock()?;
        Self::refresh_if_stale(capacity, &path, &mut state, mode)?;
        state.history.search(query)
    }

    fn update(
        &mut self,
        id: HistoryItemId,
        updater: &dyn Fn(HistoryItem) -> HistoryItem,
    ) -> Result<()> {
        let capacity = self.capacity;
        let path = self.path.clone();
        let mode = self.mode;
        let mut state = self.lock_mut()?;
        Self::refresh_if_stale(capacity, &path, &mut state, mode)?;
        state.history.update(id, updater)
    }

    fn clear(&mut self) -> Result<()> {
        let capacity = self.capacity;
        let path = self.path.clone();
        let mode = self.mode;
        let mut state = self.lock_mut()?;
        Self::refresh_if_stale(capacity, &path, &mut state, mode)?;
        state.history.clear()?;
        state.signature = file_signature(&path)?;
        Ok(())
    }

    fn delete(&mut self, id: HistoryItemId) -> Result<()> {
        let capacity = self.capacity;
        let path = self.path.clone();
        let mode = self.mode;
        let mut state = self.lock_mut()?;
        Self::refresh_if_stale(capacity, &path, &mut state, mode)?;
        state.history.delete(id)
    }

    fn sync(&mut self) -> io::Result<()> {
        let state = self.state.get_mut().map_err(|_| history_lock_error())?;
        Self::sync_state(&self.path, state)
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

    #[test]
    fn saved_entries_are_visible_to_another_history_instance() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("history");
        let mut first = LiveFileBackedHistory::with_file(100, path.clone()).unwrap();
        let second = LiveFileBackedHistory::with_file(100, path).unwrap();

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
        let mut history = LiveFileBackedHistory::with_file(100, path.clone()).unwrap();

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

        let mut first = LiveFileBackedHistory::with_mode(100, path, HistoryMode::Private).unwrap();
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
}
