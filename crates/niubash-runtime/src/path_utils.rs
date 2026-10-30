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

/// The host's ONE logical shell-path resolver (niubash#627 G2): the
/// completion walk and the syntax highlighter used to carry a private
/// copy each, and both read the *process* environment's temp dir for
/// the `/tmp` arm, so a session-level `TMPDIR=/x` never moved them —
/// while the engine's own resolution (`Executor::resolve_shell_path`)
/// reads the executor's session env and follows the assignment
/// immediately. There is now exactly this definition; both readers
/// call it and no third copy may grow.
///
/// `tmp_base` is the backing directory for the session's `/tmp`: the
/// engine-resolved value the host syncs out of the Session (Shell's
/// completion-state sync stores `Executor::resolve_shell_path("/tmp")`
/// per line). Callers without session state pass
/// `std::env::temp_dir()` — the process environment is the fallback
/// only for the not-yet-synced/stateless case, never the source once a
/// session exists. `/var/tmp` deliberately keeps deriving from the
/// process temp dir: the engine's var-tmp base
/// (`windows_var_tmp_dir`) does not follow the session TMPDIR either,
/// so both layers keep it on the process source and it never drifts
/// from the open points.
pub(crate) fn resolve_logical_shell_path(value: &str, tmp_base: &Path) -> Option<PathBuf> {
    let normalized = value.replace('\\', "/");
    let root = std::env::var_os("NIU_ROOT").map(PathBuf::from)?;
    if normalized == "/" {
        return Some(root);
    }
    if normalized == "/home" || normalized.starts_with("/home/") {
        let users = shell_home_dir()?.parent()?.to_path_buf();
        let rest = normalized
            .trim_start_matches("/home")
            .trim_start_matches('/');
        return Some(if rest.is_empty() {
            users
        } else {
            users.join(rest)
        });
    }
    // /tmp and /var/tmp are per-user temporary namespaces backed by the real
    // Windows temp dir, never the install tree (unixwin/niubash#94).
    if normalized == "/tmp" || normalized.starts_with("/tmp/") {
        let rest = normalized
            .trim_start_matches("/tmp")
            .trim_start_matches('/');
        return Some(if rest.is_empty() {
            tmp_base.to_path_buf()
        } else {
            tmp_base.join(rest)
        });
    }
    if normalized == "/var/tmp" || normalized.starts_with("/var/tmp/") {
        let rest = normalized
            .trim_start_matches("/var/tmp")
            .trim_start_matches('/');
        let base = std::env::temp_dir().join("var").join("tmp");
        return Some(if rest.is_empty() {
            base
        } else {
            base.join(rest)
        });
    }
    let rest = normalized.strip_prefix('/')?;
    match rest.split('/').next() {
        Some("bin" | "dev" | "etc" | "opt" | "usr" | "var") => Some(root.join(rest)),
        _ => None,
    }
}

/// Split a shell PATH list into entries. `;` always separates; `:` separates
/// everywhere except right after a single ASCII letter at an entry start
/// (a Windows drive prefix like `C:/...` or `C:\...`).
pub(crate) fn split_shell_path_list(value: &str) -> Vec<String> {
    let bytes = value.as_bytes();
    let mut entries = Vec::new();
    let mut entry_start = 0;
    for (index, byte) in bytes.iter().enumerate() {
        let is_separator = match byte {
            b';' => true,
            b':' => !is_windows_drive_colon(bytes, entry_start, index),
            _ => false,
        };
        if is_separator {
            if entry_start < index {
                entries.push(value[entry_start..index].to_string());
            }
            entry_start = index + 1;
        }
    }
    if entry_start < value.len() {
        entries.push(value[entry_start..].to_string());
    }
    entries
}

fn is_windows_drive_colon(bytes: &[u8], entry_start: usize, colon: usize) -> bool {
    cfg!(windows)
        && colon == entry_start + 1
        && bytes[entry_start].is_ascii_alphabetic()
        && bytes
            .get(colon + 1)
            .is_some_and(|next| matches!(next, b'\\' | b'/'))
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

    /// niubash#627 G2: the unified resolver roots `/tmp` in the
    /// caller-supplied session base (the engine-resolved TMPDIR
    /// backing), NOT in the process temp dir — a session
    /// `TMPDIR=/x` assignment is exactly what changes the supplied
    /// base. `/var/tmp` deliberately stays on the process source,
    /// matching the engine, and the enclave arms are untouched.
    #[test]
    fn issue627_g2_tmp_arm_roots_in_the_session_base() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let temp = unique_temp_dir("g2-session-base");
        let root = temp.join("root");
        let session_base = temp.join("session-temp");
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(&session_base).unwrap();
        let _root = EnvGuard::set("NIU_ROOT", &root);

        assert_eq!(
            resolve_logical_shell_path("/tmp", &session_base).as_deref(),
            Some(session_base.as_path())
        );
        assert_eq!(
            resolve_logical_shell_path("/tmp/deep/file.txt", &session_base).as_deref(),
            Some(session_base.join("deep").join("file.txt").as_path())
        );
        // Even when the supplied base IS the process temp the arm
        // answers through it (one code path, no special case).
        let process = std::env::temp_dir();
        assert_eq!(
            resolve_logical_shell_path("/tmp", &process).as_deref(),
            Some(process.as_path())
        );
        // /var/tmp never follows the session base (engine parity).
        assert_eq!(
            resolve_logical_shell_path("/var/tmp", &session_base).as_deref(),
            Some(process.join("var").join("tmp").as_path())
        );
        // The untouched arms keep their shapes.
        assert_eq!(
            resolve_logical_shell_path("/", &session_base).as_deref(),
            Some(root.as_path())
        );
        assert_eq!(
            resolve_logical_shell_path("/bin/ls", &session_base).as_deref(),
            Some(root.join("bin").join("ls").as_path())
        );
        let _ = fs::remove_dir_all(temp);
    }

    /// niubash#627 G2: with no shell root configured the resolver
    /// stays out entirely (the pre-existing gate), so stateless
    /// readers on an unenclaved host see plain paths, exactly as
    /// before the unification.
    #[test]
    fn issue627_g2_resolver_stays_out_without_a_root() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _root = EnvGuard {
            name: "NIU_ROOT",
            previous: std::env::var_os("NIU_ROOT"),
        };
        std::env::remove_var("NIU_ROOT");
        let session_base = unique_temp_dir("g2-no-root");
        assert_eq!(resolve_logical_shell_path("/tmp", &session_base), None);
        let _ = fs::remove_dir_all(session_base);
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
