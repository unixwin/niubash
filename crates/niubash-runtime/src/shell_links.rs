//! Keep the `bash.exe` / `sh.exe` faces of the shell as hardlinks to `niu.exe`.
//!
//! The shell has been reached through a 243 KB forwarding shim that set
//! `NIU_INVOKED_AS` and re-executed `niu.exe`, which costs one extra process on
//! every `bash -c` / `sh -c` invocation - the per-command tax that AI agents,
//! CI and git hooks pay (unixwin/niubash#377, measured at +12.8 ms per call).
//! The same faces are available for free as hardlinks, because the engine
//! already picks `$0`, `BASH_VERSION` and the POSIX mode from `argv[0]`, and
//! discovery now finds the installation from any entry point (the PATH entry
//! directory, a package payload, the directories editors put on PATH).
//!
//! Hardlinks are created **where the directories exist** (`<root>\bin`,
//! `<root>\winuxcmd\bin`, `<root>\winuxcmd\usr\bin`) rather than in the
//! distribution itself: a zip stores the 12 MB payload once per name, while
//! hardlinks on the user's machine are free. The forwarding shim stays the
//! fallback for layouts without hardlink support (exFAT, a network share), so
//! the faces keep working either way; `--relink-shells` is idempotent and is
//! re-run by the installer after an update replaces `niu.exe` (a replaced
//! `niu.exe` otherwise leaves the names pointing at the previous inode, exactly
//! like package links after a while - see unixwin/WinuxCmd#1194 for the parking
//! technique used here).

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

/// The faces of the shell that ship today, in the order they are reported.
const SHELL_FACES: [&str; 2] = ["bash.exe", "sh.exe"];

/// Directories that end up on a user's or an editor's PATH, relative to the
/// installation root.
const PATH_DIRECTORIES: [&str; 3] = ["bin", "winuxcmd/bin", "winuxcmd/usr/bin"];

/// What one `--relink-shells` run did, so callers can report it honestly.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct ShellLinkReport {
    /// Links that were created (or refreshed).
    pub created: Vec<PathBuf>,
    /// Links that already pointed at this `niu.exe`.
    pub already_current: Vec<PathBuf>,
    /// Names left alone, with the reason.
    pub skipped: Vec<(PathBuf, String)>,
    /// Names that could not be handled, with the error.
    pub failed: Vec<(PathBuf, String)>,
}

impl ShellLinkReport {
    pub fn changed(&self) -> bool {
        !self.created.is_empty()
    }
}

/// How far above the running executable the installation root may sit. The
/// shell can be reached as a hardlink in `<root>\bin`, in
/// `<root>\winuxcmd\bin` or in a package payload, so the root has to be found
/// rather than assumed.
const MAX_ROOT_ANCESTORS: usize = 4;

/// The installation root of a running shell: the directory that holds
/// `niu.exe` next to the bundled `winuxcmd` tree. Falls back to the
/// executable's own directory when nothing matches, so a portable layout
/// simply reports the directories it does not have.
pub fn find_installation_root(exe: &Path) -> PathBuf {
    let start = exe.parent().map(Path::to_path_buf).unwrap_or_default();
    let mut current = Some(start.clone());
    let mut level = 0;
    while let Some(directory) = current {
        if directory.join("niu.exe").is_file() && directory.join("winuxcmd").is_dir() {
            return directory;
        }
        if level >= MAX_ROOT_ANCESTORS {
            break;
        }
        current = directory
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty() && *parent != directory)
            .map(Path::to_path_buf);
        level += 1;
    }
    start
}

/// Every `<dir>/<face>` the installation could carry, whether or not the
/// directory exists.
pub fn planned_shell_links(root: &Path, niu: &Path) -> Vec<PathBuf> {
    let _ = niu;
    PATH_DIRECTORIES
        .iter()
        .flat_map(|dir| {
            SHELL_FACES
                .iter()
                .map(move |face| root.join(dir).join(face))
        })
        .collect()
}

/// Create (or refresh) the shell faces as hardlinks to `niu`.
///
/// Without `force`, an existing name that is *not* this `niu.exe` is left
/// alone - that is the shipped shim, or a user's own `bash.exe`, and silently
/// replacing it would be a surprise. With `force`, such a name is parked the
/// way `wpm` parks a held payload file: renamed aside, replaced by the link,
/// and only then dropped, so a shell that is running right now keeps its bytes.
pub fn link_shell_faces(root: &Path, niu: &Path, force: bool) -> ShellLinkReport {
    let mut report = ShellLinkReport::default();
    for link in planned_shell_links(root, niu) {
        let Some(directory) = link.parent() else {
            continue;
        };
        if !directory.is_dir() {
            report
                .skipped
                .push((link, "directory is not present".to_string()));
            continue;
        }
        if same_file(&link, niu) {
            report.already_current.push(link);
            continue;
        }
        match std::fs::hard_link(niu, &link) {
            Ok(()) => report.created.push(link),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                if !force {
                    report.skipped.push((
                        link,
                        "exists and is not this niu.exe; pass --force to replace it".to_string(),
                    ));
                    continue;
                }
                match replace_with_link(niu, &link) {
                    Ok(()) => report.created.push(link),
                    Err(error) => report.failed.push((link, error.to_string())),
                }
            }
            Err(error) => report.failed.push((link, error.to_string())),
        }
    }
    report
}

/// Park `link`, point it at `source`, then drop the parked name (the parking
/// step is what makes this safe while a process still runs from `link`).
fn replace_with_link(source: &Path, link: &Path) -> Result<()> {
    let mut parked_name = link
        .file_name()
        .map(|name| name.to_os_string())
        .unwrap_or_default();
    parked_name.push(format!(".niu-old-{}", std::process::id()));
    let parked = link.with_file_name(parked_name);

    let _ = std::fs::remove_file(&parked);
    std::fs::rename(link, &parked).with_context(|| {
        format!(
            "could not move the existing '{}' aside; close programs running from it and retry",
            link.display()
        )
    })?;
    match std::fs::hard_link(source, link) {
        Ok(()) => {
            let _ = std::fs::remove_file(&parked);
            Ok(())
        }
        Err(error) => {
            // Put the original back rather than leaving the name missing.
            let _ = std::fs::rename(&parked, link);
            Err(error).with_context(|| format!("could not link '{}'", link.display()))
        }
    }
}

/// Whether two names refer to the same file. Hardlinks share one identity per
/// volume, which is what makes "already the right link" answerable. The
/// standard library exposes that identity only behind the unstable
/// `windows_by_handle` feature, so the Windows side asks the filesystem
/// directly.
#[cfg(windows)]
fn same_file(a: &Path, b: &Path) -> bool {
    matches!((file_identity(a), file_identity(b)), (Some(x), Some(y)) if x == y)
}

#[cfg(windows)]
fn file_identity(path: &Path) -> Option<(u32, u32, u32)> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION, FILE_READ_ATTRIBUTES,
        FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
    };

    let wide: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    // SAFETY: `wide` is NUL-terminated and outlives the call, the handle is
    // closed on both paths, and the out-parameter points at a live local.
    unsafe {
        let handle = CreateFileW(
            wide.as_ptr(),
            FILE_READ_ATTRIBUTES,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            std::ptr::null(),
            OPEN_EXISTING,
            0,
            std::ptr::null_mut(),
        );
        if handle == INVALID_HANDLE_VALUE {
            return None;
        }
        let mut info: BY_HANDLE_FILE_INFORMATION = std::mem::zeroed();
        let ok = GetFileInformationByHandle(handle, &mut info);
        CloseHandle(handle);
        (ok != 0).then_some((
            info.dwVolumeSerialNumber,
            info.nFileIndexHigh,
            info.nFileIndexLow,
        ))
    }
}

#[cfg(not(windows))]
fn same_file(a: &Path, b: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    let (Ok(meta_a), Ok(meta_b)) = (std::fs::metadata(a), std::fs::metadata(b)) else {
        return false;
    };
    meta_a.dev() == meta_b.dev() && meta_a.ino() == meta_b.ino()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unique_temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "niubash-shell-links-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or_default()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn fake_install(tag: &str) -> (PathBuf, PathBuf) {
        let root = unique_temp_dir(tag);
        let niu = root.join("niu.exe");
        std::fs::write(&niu, b"niu payload").unwrap();
        for dir in PATH_DIRECTORIES {
            std::fs::create_dir_all(root.join(dir)).unwrap();
        }
        (root, niu)
    }

    #[test]
    fn installation_root_is_found_from_a_hardlinked_entry_name() {
        let root = unique_temp_dir("root-from-link");
        std::fs::create_dir_all(root.join("winuxcmd").join("usr").join("bin")).unwrap();
        std::fs::create_dir_all(root.join("bin")).unwrap();
        std::fs::write(root.join("niu.exe"), b"niu payload").unwrap();
        std::fs::write(root.join("winuxcmd/usr/bin/winuxcmd.exe"), b"dispatcher").unwrap();

        // Reached as the real binary, from the PATH entry directory, and from
        // inside the winuxcmd tree: all three report the installation root.
        for exe in [
            root.join("niu.exe"),
            root.join("bin/bash.exe"),
            root.join("winuxcmd/bin/sh.exe"),
        ] {
            assert_eq!(find_installation_root(&exe), root, "{}", exe.display());
        }

        // A tree without a bundled winuxcmd reports the executable's directory.
        let portable = unique_temp_dir("root-portable");
        let portable_niu = portable.join("niu.exe");
        std::fs::write(&portable_niu, b"niu payload").unwrap();
        assert_eq!(find_installation_root(&portable_niu), portable);

        let _ = std::fs::remove_dir_all(root);
        let _ = std::fs::remove_dir_all(portable);
    }

    #[test]
    fn planned_links_cover_every_path_directory() {
        let root = Path::new(r"C:\apps\Niubash");
        let links = planned_shell_links(root, Path::new(r"C:\apps\Niubash\niu.exe"));
        assert_eq!(links.len(), 6);
        assert!(links.contains(&PathBuf::from(r"C:\apps\Niubash\bin\bash.exe")));
        assert!(links.contains(&PathBuf::from(r"C:\apps\Niubash\bin\sh.exe")));
        assert!(links.contains(&PathBuf::from(r"C:\apps\Niubash\winuxcmd\bin\bash.exe")));
        assert!(links.contains(&PathBuf::from(r"C:\apps\Niubash\winuxcmd\usr\bin\sh.exe")));
    }

    #[test]
    fn linking_is_idempotent_and_refreshes_stale_names() {
        let (root, niu) = fake_install("refresh");

        let first = link_shell_faces(&root, &niu, false);
        assert_eq!(first.created.len(), 6, "{first:?}");
        assert!(first.failed.is_empty(), "{first:?}");
        for link in planned_shell_links(&root, &niu) {
            assert!(same_file(&link, &niu), "{} is not niu.exe", link.display());
        }

        // A second run has nothing to do.
        let second = link_shell_faces(&root, &niu, false);
        assert!(second.created.is_empty(), "{second:?}");
        assert_eq!(second.already_current.len(), 6, "{second:?}");

        // A stale name (the shipped shim, or a link left on the previous inode
        // after an update replaced niu.exe) is kept without --force ...
        let stale = root.join("bin").join("bash.exe");
        std::fs::remove_file(&stale).unwrap();
        std::fs::write(&stale, b"stale shim").unwrap();
        let kept = link_shell_faces(&root, &niu, false);
        assert!(kept.created.is_empty(), "{kept:?}");
        assert_eq!(kept.skipped.len(), 1, "{kept:?}");
        assert_eq!(std::fs::read(&stale).unwrap(), b"stale shim");

        // ... and replaced with --force, without leaving anything behind.
        let forced = link_shell_faces(&root, &niu, true);
        assert_eq!(forced.created.len(), 1, "{forced:?}");
        assert!(forced.failed.is_empty(), "{forced:?}");
        assert!(same_file(&stale, &niu));
        let leftovers: Vec<_> = std::fs::read_dir(root.join("bin"))
            .unwrap()
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name.contains(".niu-old-"))
            .collect();
        assert!(
            leftovers.is_empty(),
            "parked names left behind: {leftovers:?}"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn directories_that_do_not_exist_are_skipped() {
        let root = unique_temp_dir("portable");
        let niu = root.join("niu.exe");
        std::fs::write(&niu, b"niu payload").unwrap();
        std::fs::create_dir_all(root.join("bin")).unwrap();

        let report = link_shell_faces(&root, &niu, false);
        assert_eq!(report.created.len(), 2, "{report:?}");
        assert_eq!(report.skipped.len(), 4, "{report:?}");
        assert!(report
            .skipped
            .iter()
            .all(|(_, reason)| reason.contains("not present")));

        let _ = std::fs::remove_dir_all(&root);
    }
}
