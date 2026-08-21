//! Live, file-backed history for the interactive Reedline session.

use std::{
    fs, io,
    path::{Path, PathBuf},
    sync::{Mutex, MutexGuard},
    time::SystemTime,
};

use reedline::{
    FileBackedHistory, History, HistoryItem, HistoryItemId, HistorySessionId, ReedlineError,
    Result, SearchQuery,
};

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
    pub(crate) fn with_file(capacity: usize, path: PathBuf) -> Result<Self> {
        Ok(Self {
            inner: LiveFileBackedHistory::with_file(capacity, path)?,
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
}

impl LiveFileBackedHistory {
    pub(crate) fn with_file(capacity: usize, path: PathBuf) -> Result<Self> {
        let history = FileBackedHistory::with_file(capacity, path.clone())?;
        let signature = file_signature(&path)?;
        Ok(Self {
            capacity,
            path,
            state: Mutex::new(HistoryState { history, signature }),
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

    fn refresh_if_stale(capacity: usize, path: &Path, state: &mut HistoryState) -> Result<()> {
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
        let mut state = self.lock_mut()?;
        Self::refresh_if_stale(capacity, &path, &mut state)?;
        let saved = state.history.save(item)?;
        Self::sync_state(&path, &mut state)?;
        Ok(saved)
    }

    fn load(&self, id: HistoryItemId) -> Result<HistoryItem> {
        let capacity = self.capacity;
        let path = self.path.clone();
        let mut state = self.lock()?;
        Self::refresh_if_stale(capacity, &path, &mut state)?;
        state.history.load(id)
    }

    fn count(&self, query: SearchQuery) -> Result<i64> {
        let capacity = self.capacity;
        let path = self.path.clone();
        let mut state = self.lock()?;
        Self::refresh_if_stale(capacity, &path, &mut state)?;
        state.history.count(query)
    }

    fn search(&self, query: SearchQuery) -> Result<Vec<HistoryItem>> {
        let capacity = self.capacity;
        let path = self.path.clone();
        let mut state = self.lock()?;
        Self::refresh_if_stale(capacity, &path, &mut state)?;
        state.history.search(query)
    }

    fn update(
        &mut self,
        id: HistoryItemId,
        updater: &dyn Fn(HistoryItem) -> HistoryItem,
    ) -> Result<()> {
        let capacity = self.capacity;
        let path = self.path.clone();
        let mut state = self.lock_mut()?;
        Self::refresh_if_stale(capacity, &path, &mut state)?;
        state.history.update(id, updater)
    }

    fn clear(&mut self) -> Result<()> {
        let capacity = self.capacity;
        let path = self.path.clone();
        let mut state = self.lock_mut()?;
        Self::refresh_if_stale(capacity, &path, &mut state)?;
        state.history.clear()?;
        state.signature = file_signature(&path)?;
        Ok(())
    }

    fn delete(&mut self, id: HistoryItemId) -> Result<()> {
        let capacity = self.capacity;
        let path = self.path.clone();
        let mut state = self.lock_mut()?;
        Self::refresh_if_stale(capacity, &path, &mut state)?;
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
}
