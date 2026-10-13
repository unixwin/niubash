//! Shared Windows-native path helpers.
//!
//! Niubash accepts both native Windows paths (`C:/Users/me`) and compatibility
//! slash-drive paths (`/c/Users/me`). Host-facing code should normalize those
//! inputs before comparing or writing filesystem paths.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// The single home resolver every niu subsystem shares (config, mirrors,
/// the plugin rc, doctor, prompt, the setup wizard — niubash#302). The
/// `HOME` environment variable wins on EVERY platform: setting `HOME` is
/// how tests, CI, and portable setups sandbox the whole product, and an
/// explicit override must never lose to the system profile. The Windows
/// `USERPROFILE` convention is the fallback, so a default Windows session
/// (no `HOME` set) resolves exactly where it always has; on Unix the
/// classic `HOME`-first convention is unchanged.
pub(crate) fn shell_home_dir() -> Option<PathBuf> {
    ["HOME", "USERPROFILE"]
        .into_iter()
        .find_map(home_env_path)
        .or_else(dirs::home_dir)
        .map(normalize_existing_host_path)
}

pub(crate) fn home_env_path(name: &str) -> Option<PathBuf> {
    let value = std::env::var_os(name).filter(|value| !value.is_empty())?;
    Some(PathBuf::from(shell_path_to_host_path(
        value.to_string_lossy().as_ref(),
    )))
}

pub(crate) fn shell_path_to_host_path(value: &str) -> String {
    let normalized = value.replace('\\', "/");
    if cfg!(windows) {
        let bytes = normalized.as_bytes();
        if bytes.len() == 2 && bytes[0] == b'/' && (bytes[1] as char).is_ascii_alphabetic() {
            let drive = (bytes[1] as char).to_ascii_uppercase();
            return format!("{drive}:/");
        }
        if bytes.len() >= 3
            && bytes[0] == b'/'
            && (bytes[1] as char).is_ascii_alphabetic()
            && bytes[2] == b'/'
        {
            let drive = (bytes[1] as char).to_ascii_uppercase();
            return format!("{drive}:{}", &normalized[2..]);
        }
    }
    value.to_string()
}

pub(crate) fn normalize_existing_host_path(path: PathBuf) -> PathBuf {
    let Ok(canonical) = std::fs::canonicalize(&path) else {
        return path;
    };
    #[cfg(windows)]
    {
        normalize_windows_extended_path(canonical).unwrap_or(path)
    }
    #[cfg(not(windows))]
    {
        canonical
    }
}

#[cfg(windows)]
fn normalize_windows_extended_path(path: PathBuf) -> Option<PathBuf> {
    let path = path.to_string_lossy();
    let without_prefix = path
        .strip_prefix(r"\\?\UNC\")
        .map(|rest| format!(r"\\{rest}"))
        .or_else(|| path.strip_prefix(r"\\?\").map(str::to_string))
        .unwrap_or_else(|| path.to_string());
    Some(PathBuf::from(without_prefix))
}

/// Monotonic-per-process suffix so two writers never share a temp name
/// (a shared name would let a second writer clobber the first's staging
/// file between write and rename).
fn atomic_seq() -> u128 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    (COUNTER.fetch_add(1, Ordering::Relaxed) as u128) << 32 | std::process::id() as u128
}

/// Replace-rename: `from` atomically becomes `to`, replacing whatever
/// `to` held — Windows `MoveFileExW(MOVEFILE_REPLACE_EXISTING)` semantics
/// (std's rename corresponds to exactly that flag), POSIX `rename(2)`
/// elsewhere. One syscall either way, so a concurrent reader of `to` sees
/// either the old or the new full content and never a missing window
/// (niubash#304).
///
/// Access-denied is retried briefly: a reader that holds the target open
/// at the rename instant can make the legacy `MoveFileExW` path report
/// `ERROR_ACCESS_DENIED` even though every opener shared delete; the
/// handle closes between reads, so a short bounded retry lands in the gap
/// (and rides out transient antivirus/indexer holds too).
fn rename_replace(from: &Path, to: &Path) -> io::Result<()> {
    const RETRIES: usize = 25;
    let mut attempt = 1;
    loop {
        match std::fs::rename(from, to) {
            Ok(()) => return Ok(()),
            Err(error)
                if attempt < RETRIES
                    && matches!(
                        error.raw_os_error(),
                        Some(5) | Some(32) // ERROR_ACCESS_DENIED | ERROR_SHARING_VIOLATION
                    ) =>
            {
                std::thread::sleep(std::time::Duration::from_millis(
                    attempt.try_into().unwrap_or(25),
                ));
                attempt += 1;
            }
            Err(error) => return Err(error),
        }
    }
}

/// Write `data` to `path` atomically (niubash#304): the bytes land in a
/// uniquely-named sibling temp file first, then a single replace-rename
/// swaps it in. A concurrent reader of `path` therefore always observes a
/// complete file — the truncate-in-place window (`fs::write` over an
/// existing file) and the absent window (remove-then-rename) are both
/// gone. The temp sibling shares the target's directory, so the rename can
/// never cross a filesystem boundary.
pub(crate) fn write_file_atomic(path: &Path, data: &[u8]) -> io::Result<()> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(dir)?;
    let file_name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "file".to_string());
    let tmp = dir.join(format!(".{file_name}.tmp-{}", atomic_seq()));
    let write = fs::write(&tmp, data).and_then(|()| rename_replace(&tmp, path));
    if write.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    write
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::PROCESS_STATE_LOCK;

    struct EnvGuard {
        name: &'static str,
        previous: Option<std::ffi::OsString>,
    }

    impl EnvGuard {
        fn set(name: &'static str, value: &Path) -> Self {
            let previous = std::env::var_os(name);
            std::env::set_var(name, value);
            Self { name, previous }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match &self.previous {
                Some(value) => std::env::set_var(self.name, value),
                None => std::env::remove_var(self.name),
            }
        }
    }

    fn unique_temp_dir(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "niu-pathutils-{}-{}-{}",
            label,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// niubash#302: `HOME` alone must sandbox the whole product. With
    /// `HOME` pointing at an isolated dir and `USERPROFILE` deliberately
    /// left at a DIFFERENT dir, the shared resolver has to pick `HOME` —
    /// the explicit override never loses to the system profile on any
    /// platform (the default Windows session, `HOME` unset, still falls
    /// through to `USERPROFILE` exactly as before).
    #[test]
    fn home_env_overrides_userprofile_for_isolation() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let temp = unique_temp_dir("home-first");
        let isolated = temp.join("isolated-home");
        let profile = temp.join("real-profile");
        fs::create_dir_all(&isolated).unwrap();
        fs::create_dir_all(&profile).unwrap();
        let _home = EnvGuard::set("HOME", &isolated);
        let _userprofile = EnvGuard::set("USERPROFILE", &profile);
        assert_eq!(
            shell_home_dir().unwrap(),
            normalize_existing_host_path(isolated)
        );
    }

    /// The `HOME`-unset fallback keeps the Windows `USERPROFILE` convention
    /// (default home semantics unchanged — niubash#302 only re-ranks an
    /// explicit override).
    #[test]
    fn userprofile_is_the_fallback_when_home_is_unset() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let temp = unique_temp_dir("userprofile-fallback");
        let profile = temp.join("profile");
        fs::create_dir_all(&profile).unwrap();
        let _home = EnvGuard {
            name: "HOME",
            previous: std::env::var_os("HOME"),
        };
        std::env::remove_var("HOME");
        let _userprofile = EnvGuard::set("USERPROFILE", &profile);
        assert_eq!(
            shell_home_dir().unwrap(),
            normalize_existing_host_path(profile)
        );
    }

    /// niubash#304: the atomic writer replaces existing content in one
    /// rename (Windows: MoveFileExW REPLACE_EXISTING) and leaves no temp
    /// sibling behind, so a path that already exists is swapped, not
    /// truncated.
    #[test]
    fn write_file_atomic_replaces_existing_content() {
        let temp = unique_temp_dir("atomic-replace");
        let path = temp.join("target.toml");
        fs::write(&path, "the-old-full-content\n").unwrap();
        write_file_atomic(&path, b"the-new-full-content\n").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "the-new-full-content\n");
        let leftovers: Vec<_> = fs::read_dir(&temp)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| name.contains(".tmp-"))
            .collect();
        assert!(leftovers.is_empty(), "temp siblings leaked: {leftovers:?}");
        let _ = fs::remove_dir_all(temp);
    }

    /// A write into a not-yet-existing directory tree creates it (the rc
    /// writers rely on this for a first-run home).
    #[test]
    fn write_file_atomic_creates_missing_parent_dirs() {
        let temp = unique_temp_dir("atomic-mkdir");
        let path = temp.join(".niubash").join("config.toml");
        write_file_atomic(&path, b"schema = \"x\"\n").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "schema = \"x\"\n");
        let _ = fs::remove_dir_all(temp);
    }
}
