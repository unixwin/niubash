//! Live, file-backed history for the interactive Reedline session.

use std::{
    collections::VecDeque,
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
    sync::Mutex,
    time::SystemTime,
};

use reedline::{
    FileBackedHistory, History, HistoryItem, HistoryItemId, HistorySessionId, ReedlineError,
    Result, SearchQuery,
};

use crate::config::HistoryMode;

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
    pub(crate) fn with_file(
        capacity: usize,
        path: PathBuf,
        mode: HistoryMode,
    ) -> Result<Self> {
        Ok(Self {
            inner: LiveFileBackedHistory::with_mode(capacity, path, mode)?,
        })
    }
}

impl rubash::history::HistoryProvider for RubashHistoryProvider {
    fn entries(&mut self) -> io::Result<Vec<String>> {
        let items = self
            .inner
            .realtime_items()
            .map_err(|error| io::Error::other(error.to_string()))?;
        Ok(items)
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

/// Shared state: delegated to FileBackedHistory (live cross-shell sharing)
struct SharedState {
    history: FileBackedHistory,
    signature: FileSignature,
}

/// Fixed state: in-memory navigation with append-only file writes
struct FixedState {
    nav: VecDeque<String>,
    path: PathBuf,
    signature: FileSignature,
    last_saved: VecDeque<String>, // Track what we've already saved
}

enum HistoryBackend {
    Shared(SharedState),
    Fixed(FixedState),
}

/// A Reedline file history that supports three modes:
/// - Shared: live cross-shell sharing (default, backward compatible)
/// - Session: stable navigation snapshot at startup + own commands; builtins see live updates
/// - Private: isolated history - only this session's commands
pub(crate) struct LiveFileBackedHistory {
    capacity: usize,
    mode: HistoryMode,
    backend: Mutex<HistoryBackend>,
}

impl LiveFileBackedHistory {
    pub(crate) fn with_mode(capacity: usize, path: PathBuf, mode: HistoryMode) -> Result<Self> {
        let signature = file_signature(&path)?;
        let backend = match mode {
            HistoryMode::Shared => {
                let history = FileBackedHistory::with_file(capacity, path.clone())?;
                HistoryBackend::Shared(SharedState {
                    history,
                    signature,
                })
            }
            HistoryMode::Session | HistoryMode::Private => {
                // For Session/Private, load initial snapshot from file
                let nav = read_history_tail(&path, capacity)?;
                HistoryBackend::Fixed(FixedState {
                    nav,
                    path,
                    signature,
                    last_saved: VecDeque::new(),
                })
            }
        };
        Ok(Self {
            capacity,
            mode,
            backend: Mutex::new(backend),
        })
    }

    fn lock_mut(&self) -> Result<MutexGuard<'_, HistoryBackend>, ReedlineError> {
        self.backend
            .lock()
            .map_err(|_| ReedlineError::IOError(io::Error::new(
                io::ErrorKind::Other,
                HISTORY_LOCK_ERROR,
            )))
    }
}

impl History for LiveFileBackedHistory {
    fn save(&mut self, item: HistoryItem) -> Result<Option<HistoryItemId>> {
        let command = item.command_line.clone();
        
        // Don't save empty commands
        if command.trim().is_empty() {
            return Ok(None);
        }

        let backend = self.lock_mut()?;
        match &mut *backend {
            HistoryBackend::Shared(state) => {
                let id = state.history.save(item)?;
                state.history.sync()?;
                state.signature = file_signature(&state.history.path())?;
                Ok(id)
            }
            HistoryBackend::Fixed(state) => {
                // Deduplicate: don't save if same as last command
                if let Some(last) = state.nav.back() {
                    if last == &command {
                        return Ok(None);
                    }
                }

                // Add to in-memory navigation
                state.nav.push_back(command.clone());
                state.last_saved.push_back(command.clone());

                // Trim to capacity
                while state.nav.len() > self.capacity {
                    state.nav.pop_front();
                }

                // Append to file
                append_history_line(&state.path, &command)?;

                // Update signature
                state.signature = file_signature(&state.path)?;

                // Generate a virtual ID (for Session/Private modes)
                Ok(Some(HistoryItemId::new(state.nav.len() as u64)))
            }
        }
    }

    fn load(&mut self, _max: usize) -> Result<Vec<HistoryItem>> {
        // For Shared, FileBackedHistory handles loading
        // For Fixed, we already loaded the snapshot at startup
        Ok(Vec::new())
    }

    fn search(&self, query: SearchQuery<'_>) -> Result<Vec<HistoryItem>> {
        let backend = self.lock_mut()?;
        match &*backend {
            HistoryBackend::Shared(state) => {
                // For Shared mode, check if file changed and reload
                if let Some(new_signature) = file_signature(&state.history.path()).ok() {
                    if new_signature != state.signature {
                        drop(backend);
                        // Reload from file
                        return self.search_with_reload(query);
                    }
                }
                state.history.search(query)
            }
            HistoryBackend::Fixed(state) => {
                // For Fixed modes, search only in in-memory nav
                search_fixed(&state.nav, query, self.capacity)
            }
        }
    }

    fn clear(&mut self) -> Result<()> {
        let backend = self.lock_mut()?;
        match &mut *backend {
            HistoryBackend::Shared(state) => {
                state.history.clear()?;
                state.history.sync()?;
                state.signature = file_signature(&state.history.path())?;
                Ok(())
            }
            HistoryBackend::Fixed(state) => {
                state.nav.clear();
                state.last_saved.clear();
                // Don't clear the file in Session/Private mode
                Ok(())
            }
        }
    }
}

impl LiveFileBackedHistory {
    fn search_with_reload(&self, query: SearchQuery<'_>) -> Result<Vec<HistoryItem>> {
        let mut backend = self.lock_mut()?;
        match &mut *backend {
            HistoryBackend::Shared(state) => {
                // FileBackedHistory doesn't support reload, so we recreate it
                let path = state.history.path().clone();
                let new_history = FileBackedHistory::with_file(self.capacity, path.clone())?;
                state.history = new_history;
                state.signature = file_signature(&path)?;
                state.history.search(query)
            }
            _ => unreachable!(),
        }
    }

    /// Return commands in chronological order (oldest → newest).
    /// Used by builtins (history, fc) via RubashHistoryProvider.
    pub(crate) fn realtime_items(&mut self) -> Result<Vec<String>> {
        let backend = self.lock_mut()?;
        match &*backend {
            HistoryBackend::Shared(state) => {
                // Read all items from file in chronological order
                let query = SearchQuery::everything(SearchDirection::Forward, None);
                let items = state.history.search(query)?;
                Ok(items.into_iter().map(|item| item.command_line).collect())
            }
            HistoryBackend::Fixed(state) => {
                // For Session/Private, return nav in chronological order
                // But for Session mode, we should also read live updates from file
                if self.mode == HistoryMode::Session {
                    // Read current file content
                    let file_items = read_history_tail(&state.path, usize::MAX)?;
                    // Combine: file items + own items not yet saved
                    let mut result = file_items;
                    for item in state.nav.iter() {
                        if !result.contains(item) {
                            result.push_back(item.clone());
                        }
                    }
                    Ok(result.into_iter().collect())
                } else {
                    // Private mode: only own commands
                    Ok(state.nav.iter().cloned().collect())
                }
            }
        }
    }
}

fn file_signature(path: &Path) -> Result<FileSignature> {
    let metadata = fs::metadata(path);
    Ok(FileSignature {
        exists: metadata.is_ok(),
        len: metadata.as_ref().map(|m| m.len()).unwrap_or(0),
        modified: metadata.ok().and_then(|m| m.modified().ok()),
    })
}

fn read_history_tail(path: &Path, max: usize) -> Result<VecDeque<String>> {
    if !path.exists() {
        return Ok(VecDeque::new());
    }

    let content = fs::read_to_string(path)
        .map_err(|e| ReedlineError::IOError(e))?;
    
    let lines: VecDeque<String> = content
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.to_string())
        .collect();

    // Return last 'max' items
    let start = if lines.len() > max { lines.len() - max } else { 0 };
    Ok(lines.into_iter().skip(start).collect())
}

fn search_fixed(
    nav: &VecDeque<String>,
    query: SearchQuery<'_>,
    capacity: usize,
) -> Result<Vec<HistoryItem>> {
    let term = query.search_term.unwrap_or("");
    let direction = query.direction;

    // Filter and transform nav
    let mut items: Vec<HistoryItem> = nav
        .iter()
        .enumerate()
        .filter(|(_, line)| term.is_empty() || line.contains(term))
        .map(|(i, line)| {
            let id = HistoryItemId::new(i as u64 + 1);
            HistoryItem {
                id,
                start_timestamp: None,
                command_line: line.clone(),
                session_id: HistorySessionId::new(0),
                hostname: None,
            }
        })
        .collect();

    // Sort by direction
    match direction {
        SearchDirection::Forward => {
            // Already in chronological order
        }
        SearchDirection::Reverse => {
            items.reverse();
        }
        SearchDirection::Undefined => {}
    }

    // Apply limit
    if let Some(limit) = query.limit {
        items.truncate(limit);
    }

    Ok(items)
}

fn append_history_line(path: &Path, line: &str) -> io::Result<()> {
    // Windows file locking requires read+write handle with truncate(false)
    // Using fd-lock 4.0 which matches reedline's dependency version
    let mut file = fs::OpenOptions::new()
        .create(true)
        .write(true)
        .read(true)
        .truncate(false)
        .open(path)?;

    // Seek to end
    use std::io::Seek;
    file.seek(io::SeekFrom::End(0))?;

    // Lock the file (Windows: requires read+write handle)
    #[cfg(windows)]
    {
        use fd_lock::RwLock;
        let mut lock = RwLock::new(file);
        let mut guard = lock.write()?;
        writeln!(guard, "{}", line)?;
    }

    #[cfg(unix)]
    {
        writeln!(file, "{}", line)?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn shared_entries_are_visible_to_another_history_instance() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("history.txt");

        // First instance: save a command
        let mut h1 = LiveFileBackedHistory::with_mode(100, path.clone(), HistoryMode::Shared).unwrap();
        h1.save(HistoryItem::from_command_line("echo hello".to_string())).unwrap();
        h1.save(HistoryItem::from_command_line("echo world".to_string())).unwrap();

        // Second instance: should see both commands
        let mut h2 = LiveFileBackedHistory::with_mode(100, path.clone(), HistoryMode::Shared).unwrap();
        let items = h2.realtime_items().unwrap();
        assert_eq!(items.len(), 2);
        assert!(items.contains(&"echo hello".to_string()));
        assert!(items.contains(&"echo world".to_string()));
    }

    #[test]
    fn session_navigation_stays_on_the_startup_timeline_while_others_write() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("history.txt");

        // Pre-populate file
        fs::write(&path, "cmd1\ncmd2\ncmd3\n").unwrap();

        // First instance (session): loads snapshot
        let h1 = LiveFileBackedHistory::with_mode(100, path.clone(), HistoryMode::Session).unwrap();
        let query = SearchQuery::everything(SearchDirection::Forward, None);
        let items1 = h1.search(query).unwrap();
        assert_eq!(items1.len(), 3);

        // Second instance writes a new command
        let mut h2 = LiveFileBackedHistory::with_mode(100, path.clone(), HistoryMode::Session).unwrap();
        h2.save(HistoryItem::from_command_line("cmd4".to_string())).unwrap();

        // First instance navigation should still show only startup snapshot
        let items1_again = h1.search(query).unwrap();
        assert_eq!(items1_again.len(), 3);

        // But builtin (realtime_items) should see the new command
        let mut h1_mut = LiveFileBackedHistory::with_mode(100, path.clone(), HistoryMode::Session).unwrap();
        let realtime = h1_mut.realtime_items().unwrap();
        assert_eq!(realtime.len(), 4);
        assert!(realtime.contains(&"cmd4".to_string()));
    }

    #[test]
    fn private_navigation_only_shows_this_sessions_commands() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("history.txt");

        // Pre-populate file
        fs::write(&path, "cmd1\ncmd2\ncmd3\n").unwrap();

        // First instance (private): loads nothing from file
        let mut h1 = LiveFileBackedHistory::with_mode(100, path.clone(), HistoryMode::Private).unwrap();
        
        // Save own command
        h1.save(HistoryItem::from_command_line("mycmd".to_string())).unwrap();

        // Navigation should only show own command
        let query = SearchQuery::everything(SearchDirection::Forward, None);
        let items = h1.search(query).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].command_line, "mycmd");

        // realtime_items should also only show own command
        let realtime = h1.realtime_items().unwrap();
        assert_eq!(realtime.len(), 1);
        assert_eq!(realtime[0], "mycmd");
    }

    #[test]
    fn duplicated_last_commands_are_not_saved_twice_in_fixed_modes() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("history.txt");

        for mode in [HistoryMode::Session, HistoryMode::Private] {
            fs::write(&path, "").unwrap();

            let mut h = LiveFileBackedHistory::with_mode(100, path.clone(), mode).unwrap();
            
            // Save same command twice
            h.save(HistoryItem::from_command_line("echo test".to_string())).unwrap();
            h.save(HistoryItem::from_command_line("echo test".to_string())).unwrap();

            // Should only be saved once
            let content = fs::read_to_string(&path).unwrap();
            let lines: Vec<&str> = content.lines().filter(|l| !l.is_empty()).collect();
            assert_eq!(lines.len(), 1, "Failed for mode: {:?}", mode);
            assert_eq!(lines[0], "echo test");
        }
    }

    #[test]
    fn empty_and_duplicate_saves_do_not_allocate_ids_in_fixed_modes() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("history.txt");

        for mode in [HistoryMode::Session, HistoryMode::Private] {
            fs::write(&path, "").unwrap();

            let mut h = LiveFileBackedHistory::with_mode(100, path.clone(), mode).unwrap();
            
            // Try to save empty string
            let id1 = h.save(HistoryItem::from_command_line("".to_string())).unwrap();
            assert!(id1.is_none(), "Empty string should not be saved (mode: {:?})", mode);

            // Save a real command
            let id2 = h.save(HistoryItem::from_command_line("cmd".to_string())).unwrap();
            assert!(id2.is_some());

            // Try to save duplicate
            let id3 = h.save(HistoryItem::from_command_line("cmd".to_string())).unwrap();
            assert!(id3.is_none(), "Duplicate should not be saved (mode: {:?})", mode);
        }
    }
}
