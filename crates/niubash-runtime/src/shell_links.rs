//! Keep the `bash` / `sh` shell faces as hardlinks to the shell binary
//! (`niu.exe` on Windows, `niu` on Unix).
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
//!
//! niubash#464: the fallback above is implemented, not aspirational. Where a
//! hardlink cannot be created (or an existing face cannot be re-linked), the
//! face is materialized as a byte copy of the forwarding-shim payload the
//! package stages at `<root>\winuxcmd\usr\bin\niu-shim.exe`
//! ([`shim_payload_path`]), and the report counts such faces as `shimmed`
//! rather than `created`: they work, at the old one-extra-process cost, until
//! a later relink on a link-capable volume upgrades them to hardlinks. A face
//! that already carries the payload's bytes (an earlier fallback, or the
//! package's own layout) is upgraded the same way and only stays `shimmed`
//! when the upgrade itself is refused by the filesystem.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

/// The faces of the shell that ship today, in the order they are reported.
#[cfg(windows)]
const SHELL_FACES: [&str; 2] = ["bash.exe", "sh.exe"];
#[cfg(not(windows))]
const SHELL_FACES: [&str; 2] = ["bash", "sh"];

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
    /// Faces materialized as copies of the forwarding-shim payload because
    /// a hardlink could not be created (niubash#464). Working faces, at the
    /// shim's one-extra-process cost.
    pub shimmed: Vec<PathBuf>,
    /// Bytes displaced by a replace and kept under a durable
    /// `<name>.bak-<yyyymmdd>` sibling instead of the transient
    /// `.niu-old-<pid>` park (niubash#810: a standalone binary at a
    /// bundled-shim name is user-visible state, not garbage). Filled by
    /// the bundled-shim reconcile, never by the shell-face relink.
    pub backed_up: Vec<PathBuf>,
    /// Names left alone, with the reason.
    pub skipped: Vec<(PathBuf, String)>,
    /// Names that could not be handled, with the error.
    pub failed: Vec<(PathBuf, String)>,
}

impl ShellLinkReport {
    pub fn changed(&self) -> bool {
        !self.created.is_empty() || !self.shimmed.is_empty()
    }
}

/// How far above the running executable the installation root may sit. The
/// shell can be reached as a hardlink in `<root>\bin`, in
/// `<root>\winuxcmd\bin` or in a package payload, so the root has to be found
/// rather than assumed.
const MAX_ROOT_ANCESTORS: usize = 4;

/// The installed shell binary's file name on this platform. Installation
/// discovery must look for what the platform actually ships: `niu.exe` in a
/// Windows layout, `niu` on Unix (where the kernel resolves a `bash`/`sh`
/// symlink or hardlink face back to this file anyway, but a copied face still
/// has to find its way home).
#[cfg(windows)]
const SHELL_BIN_NAME: &str = "niu.exe";
#[cfg(not(windows))]
const SHELL_BIN_NAME: &str = "niu";

/// The staged forwarding-shim payload's file name (niubash#464). The
/// packager builds the `bash` bin from this repository and stages one
/// canonical copy under `winuxcmd\usr\bin`; [`link_shell_faces`] copies
/// those bytes to a face wherever a hardlink cannot exist.
#[cfg(windows)]
const SHIM_PAYLOAD_NAME: &str = "niu-shim.exe";
#[cfg(not(windows))]
const SHIM_PAYLOAD_NAME: &str = "niu-shim";

/// Where the staged forwarding-shim payload lives inside an installation.
pub fn shim_payload_path(root: &Path) -> PathBuf {
    root.join("winuxcmd")
        .join("usr")
        .join("bin")
        .join(SHIM_PAYLOAD_NAME)
}

/// The installation root of a running shell: the directory that holds
/// the shell binary next to the bundled `winuxcmd` tree. Falls back to the
/// executable's own directory when nothing matches, so a portable layout
/// simply reports the directories it does not have.
pub fn find_installation_root(exe: &Path) -> PathBuf {
    let start = exe.parent().map(Path::to_path_buf).unwrap_or_default();
    let mut current = Some(start.clone());
    let mut level = 0;
    while let Some(directory) = current {
        if directory.join(SHELL_BIN_NAME).is_file() && directory.join("winuxcmd").is_dir() {
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

/// The real shell binary of the installation the running executable belongs
/// to. Reached as a `bash`/`sh` face - a hardlink, or the forwarding shim's
/// re-exec - the running path is the face, not the shell, which is why the
/// contract variable (`NIU_SHELL`) and the shim's own target
/// search must resolve this way. Without an installation to point at, the
/// invoked path is the best answer available.
pub fn resolved_shell_path(invoked: &Path) -> PathBuf {
    let candidate = find_installation_root(invoked).join(SHELL_BIN_NAME);
    if candidate.is_file() {
        candidate
    } else {
        invoked.to_path_buf()
    }
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

/// Create (or refresh) the shell faces as hardlinks to the shell binary.
///
/// Without `force`, an existing name that is *not* this shell binary is left
/// alone - that is the shipped shim, or a user's own `bash`, and silently
/// replacing it would be a surprise. With `force`, such a name is parked the
/// way `wpm` parks a held payload file: renamed aside, replaced by the link,
/// and only then dropped, so a shell that is running right now keeps its bytes.
///
/// Where a hardlink cannot exist at all (niubash#464: exFAT, a network
/// share, a volume without link support), the face falls back to a byte
/// copy of the staged forwarding-shim payload instead of failing - see the
/// module docs.
pub fn link_shell_faces(root: &Path, niu: &Path, force: bool) -> ShellLinkReport {
    link_shell_faces_with_linker(root, niu, force, &hard_link)
}

/// `std::fs::hard_link` behind the concrete signature the relink worker
/// injects (the generic std function does not coerce to `&dyn Fn`).
fn hard_link(source: &Path, link: &Path) -> std::io::Result<()> {
    std::fs::hard_link(source, link)
}

/// The worker behind [`link_shell_faces`], with the link operation
/// injected so tests can drive the niubash#464 fallback paths on a
/// filesystem that would otherwise happily hardlink.
fn link_shell_faces_with_linker(
    root: &Path,
    niu: &Path,
    force: bool,
    hard_link: &dyn Fn(&Path, &Path) -> std::io::Result<()>,
) -> ShellLinkReport {
    let payload = shim_payload_path(root);
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
        if link.is_dir() {
            report
                .failed
                .push((link, "is a directory; refusing to replace it".to_string()));
            continue;
        }
        if same_file(&link, niu) {
            report.already_current.push(link);
            continue;
        }
        let present = std::fs::symlink_metadata(&link).is_ok();
        if present && same_file_bytes(&link, &payload) {
            // Already a working face carrying the staged shim's bytes, laid
            // down by an earlier fallback or by the package layout itself.
            // Upgrade to a hardlink where the volume allows; where it does
            // not, the face is honestly reported as shimmed, not failed.
            match replace_with_link_using(niu, &link, hard_link) {
                Ok(()) => report.created.push(link),
                Err(_) => report.shimmed.push(link),
            }
            continue;
        }
        if present && same_file_bytes(&link, niu) {
            // A byte copy of the shell itself (the zip/Inno payload shape):
            // it IS this shell, so the same upgrade applies; a refused link
            // leaves a working face that already points at the right bytes.
            match replace_with_link_using(niu, &link, hard_link) {
                Ok(()) => report.created.push(link),
                Err(_) => report.already_current.push(link),
            }
            continue;
        }
        if present && !force {
            report.skipped.push((
                link,
                format!("exists and is not this {SHELL_BIN_NAME}; pass --force to replace it"),
            ));
            continue;
        }
        if present {
            match replace_with_link_using(niu, &link, hard_link) {
                Ok(()) => report.created.push(link),
                Err(_) => match replace_with_shim_copy(&payload, &link) {
                    Ok(()) => report.shimmed.push(link),
                    Err(error) => report.failed.push((link, error.to_string())),
                },
            }
            continue;
        }
        match hard_link(niu, &link) {
            Ok(()) => report.created.push(link),
            Err(_) => match install_shim_copy(&payload, &link) {
                Ok(()) => report.shimmed.push(link),
                Err(error) => report.failed.push((link, error.to_string())),
            },
        }
    }
    report
}

/// Park `link`, point it at `source`, then drop the parked name (the parking
/// step is what makes this safe while a process still runs from `link`).
pub(crate) fn replace_with_link(source: &Path, link: &Path) -> Result<()> {
    replace_with_link_using(source, link, &hard_link)
}

fn replace_with_link_using(
    source: &Path,
    link: &Path,
    hard_link: &dyn Fn(&Path, &Path) -> std::io::Result<()>,
) -> Result<()> {
    let parked = park_existing(link)?;
    match hard_link(source, link) {
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

/// Move `link` aside under a sibling parking name and return the parked
/// path (unixwin/WinuxCmd#1194: a process still running from `link` keeps
/// its bytes under the parked name).
fn park_existing(link: &Path) -> Result<PathBuf> {
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
    Ok(parked)
}

/// Replace `link` with a byte copy of the staged shim payload (niubash#464):
/// the face keeps working - through the forwarding shim - on volumes where
/// no hardlink can exist. Same parking discipline as [`replace_with_link`],
/// so a shell running from the old face is not disturbed.
fn replace_with_shim_copy(payload: &Path, link: &Path) -> Result<()> {
    let bytes = read_shim_payload(payload)?;
    let parked = park_existing(link)?;
    match write_new_face(&bytes, link) {
        Ok(()) => {
            let _ = std::fs::remove_file(&parked);
            Ok(())
        }
        Err(error) => {
            // Put the original back rather than leaving the name missing.
            let _ = std::fs::rename(&parked, link);
            Err(error).with_context(|| format!("could not write '{}'", link.display()))
        }
    }
}

/// Lay the staged shim payload's bytes down as a new face at `link`
/// (niubash#464 fallback for a face that does not exist yet).
fn install_shim_copy(payload: &Path, link: &Path) -> Result<()> {
    let bytes = read_shim_payload(payload)?;
    write_new_face(&bytes, link).with_context(|| format!("could not write '{}'", link.display()))
}

fn read_shim_payload(payload: &Path) -> Result<Vec<u8>> {
    std::fs::read(payload).with_context(|| {
        format!(
            "could not read the staged shim payload '{}'",
            payload.display()
        )
    })
}

/// Write `bytes` as a brand-new file at `link` (`create_new`, so a face
/// that raced into existence is never clobbered), executable on Unix.
fn write_new_face(bytes: &[u8], link: &Path) -> std::io::Result<()> {
    use std::io::Write as _;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o755);
    }
    let mut file = options.open(link)?;
    file.write_all(bytes)
}

/// Whether two names refer to the same file. Hardlinks share one identity per
/// volume, which is what makes "already the right link" answerable. The
/// standard library exposes that identity only behind the unstable
/// `windows_by_handle` feature, so the Windows side asks the filesystem
/// directly.
#[cfg(windows)]
pub(crate) fn same_file(a: &Path, b: &Path) -> bool {
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
pub(crate) fn same_file(a: &Path, b: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    let (Ok(meta_a), Ok(meta_b)) = (std::fs::metadata(a), std::fs::metadata(b)) else {
        return false;
    };
    meta_a.dev() == meta_b.dev() && meta_a.ino() == meta_b.ino()
}

/// Byte-for-byte equality for the copy-fallback shape: two paths that are
/// not the same file may still carry the same image (the Inno/zip payload
/// lays every bundled name down as a copy, and the niubash#464 relink
/// fallback lays faces down as shim copies). Sizes first so the common
/// "different image" answer costs two stats instead of two full reads.
pub(crate) fn same_file_bytes(a: &Path, b: &Path) -> bool {
    let (Ok(a_meta), Ok(b_meta)) = (a.metadata(), b.metadata()) else {
        return false;
    };
    if a_meta.len() != b_meta.len() {
        return false;
    }
    let (Ok(a_bytes), Ok(b_bytes)) = (std::fs::read(a), std::fs::read(b)) else {
        return false;
    };
    a_bytes == b_bytes
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
        let niu = root.join(SHELL_BIN_NAME);
        std::fs::write(&niu, b"niu payload").unwrap();
        for dir in PATH_DIRECTORIES {
            std::fs::create_dir_all(root.join(dir)).unwrap();
        }
        (root, niu)
    }

    /// A "links work" injector for the report/upgrade logic tests. It uses
    /// the real hardlink where the host filesystem supports one; Android's
    /// app-data filesystems refuse hardlinks even inside one directory
    /// (EPERM from SELinux), so there a symlink is the equivalent
    /// same-inode face (`same_file` follows it). The product's hardlink
    /// failure path is not hidden by this: `always_fail_link` drives the
    /// shim-copy fallback tests explicitly, and Windows still requires a
    /// real hardlink.
    fn test_link(source: &Path, link: &Path) -> std::io::Result<()> {
        match std::fs::hard_link(source, link) {
            Ok(()) => Ok(()),
            Err(hardlink_error) => {
                #[cfg(unix)]
                {
                    std::os::unix::fs::symlink(source, link)
                }
                #[cfg(not(unix))]
                {
                    Err(hardlink_error)
                }
            }
        }
    }

    #[test]
    fn installation_root_is_found_from_a_hardlinked_entry_name() {
        let root = unique_temp_dir("root-from-link");
        std::fs::create_dir_all(root.join("winuxcmd").join("usr").join("bin")).unwrap();
        std::fs::create_dir_all(root.join("bin")).unwrap();
        std::fs::write(root.join(SHELL_BIN_NAME), b"niu payload").unwrap();
        std::fs::write(root.join("winuxcmd/usr/bin/winuxcmd.exe"), b"dispatcher").unwrap();

        // Reached as the real binary, from the PATH entry directory, and from
        // inside the winuxcmd tree: all three report the installation root.
        for exe in [
            root.join(SHELL_BIN_NAME),
            root.join("bin").join(SHELL_FACES[0]),
            root.join("winuxcmd/bin").join(SHELL_FACES[1]),
        ] {
            assert_eq!(find_installation_root(&exe), root, "{}", exe.display());
        }

        // A tree without a bundled winuxcmd reports the executable's directory.
        let portable = unique_temp_dir("root-portable");
        let portable_niu = portable.join(SHELL_BIN_NAME);
        std::fs::write(&portable_niu, b"niu payload").unwrap();
        assert_eq!(find_installation_root(&portable_niu), portable);

        let _ = std::fs::remove_dir_all(root);
        let _ = std::fs::remove_dir_all(portable);
    }

    #[test]
    fn resolved_shell_path_names_the_real_shell_not_the_face() {
        let root = unique_temp_dir("resolved-shell-path");
        std::fs::create_dir_all(root.join("winuxcmd").join("usr").join("bin")).unwrap();
        std::fs::create_dir_all(root.join("bin")).unwrap();
        std::fs::write(root.join(SHELL_BIN_NAME), b"niu payload").unwrap();
        std::fs::write(root.join("winuxcmd/usr/bin/winuxcmd.exe"), b"dispatcher").unwrap();
        std::fs::write(root.join("bin").join(SHELL_FACES[0]), b"niu payload").unwrap();

        // Invoked through the face, the contract still names the shell.
        assert_eq!(
            resolved_shell_path(&root.join("bin").join(SHELL_FACES[0])),
            root.join(SHELL_BIN_NAME)
        );
        assert_eq!(
            resolved_shell_path(&root.join(SHELL_BIN_NAME)),
            root.join(SHELL_BIN_NAME)
        );

        // No installation: the invoked path is what we have.
        let bare = unique_temp_dir("resolved-shell-path-bare");
        let bare_exe = bare.join(SHELL_FACES[0]);
        std::fs::write(&bare_exe, b"standalone").unwrap();
        assert_eq!(resolved_shell_path(&bare_exe), bare_exe);

        let _ = std::fs::remove_dir_all(root);
        let _ = std::fs::remove_dir_all(bare);
    }

    #[test]
    fn planned_links_cover_every_path_directory() {
        // Built with `join` so the expectations hold whatever the platform
        // separator is; the cross-target jobs compile these on unix hosts too.
        let root = PathBuf::from("apps").join("Niubash");
        let links = planned_shell_links(&root, &root.join(SHELL_BIN_NAME));
        assert_eq!(links.len(), 6);
        let cases: &[&[&str]] = &[
            &["bin", SHELL_FACES[0]],
            &["bin", SHELL_FACES[1]],
            &["winuxcmd", "bin", SHELL_FACES[0]],
            &["winuxcmd", "usr", "bin", SHELL_FACES[1]],
        ];
        for relative in cases {
            let expected = relative
                .iter()
                .fold(root.clone(), |path, part| path.join(*part));
            assert!(links.contains(&expected), "{expected:?} not in {links:?}");
        }
    }

    #[test]
    fn linking_is_idempotent_and_refreshes_stale_names() {
        let (root, niu) = fake_install("refresh");

        let first = link_shell_faces_with_linker(&root, &niu, false, &test_link);
        assert_eq!(first.created.len(), 6, "{first:?}");
        assert!(first.failed.is_empty(), "{first:?}");
        for link in planned_shell_links(&root, &niu) {
            assert!(
                same_file(&link, &niu),
                "{} is not the shell",
                link.display()
            );
        }

        // A second run has nothing to do.
        let second = link_shell_faces_with_linker(&root, &niu, false, &test_link);
        assert!(second.created.is_empty(), "{second:?}");
        assert_eq!(second.already_current.len(), 6, "{second:?}");

        // A stale name (the shipped shim, or a link left on the previous inode
        // after an update replaced the shell) is kept without --force ...
        let stale = root.join("bin").join(SHELL_FACES[0]);
        std::fs::remove_file(&stale).unwrap();
        std::fs::write(&stale, b"stale shim").unwrap();
        let kept = link_shell_faces_with_linker(&root, &niu, false, &test_link);
        assert!(kept.created.is_empty(), "{kept:?}");
        assert_eq!(kept.skipped.len(), 1, "{kept:?}");
        assert_eq!(std::fs::read(&stale).unwrap(), b"stale shim");

        // ... and replaced with --force, without leaving anything behind.
        let forced = link_shell_faces_with_linker(&root, &niu, true, &test_link);
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
        let niu = root.join(SHELL_BIN_NAME);
        std::fs::write(&niu, b"niu payload").unwrap();
        std::fs::create_dir_all(root.join("bin")).unwrap();

        let report = link_shell_faces_with_linker(&root, &niu, false, &test_link);
        assert_eq!(report.created.len(), 2, "{report:?}");
        assert_eq!(report.skipped.len(), 4, "{report:?}");
        assert!(report
            .skipped
            .iter()
            .all(|(_, reason)| reason.contains("not present")));

        let _ = std::fs::remove_dir_all(&root);
    }

    fn fake_install_with_payload(tag: &str) -> (PathBuf, PathBuf) {
        let (root, niu) = fake_install(tag);
        std::fs::write(shim_payload_path(&root), b"shim payload").unwrap();
        (root, niu)
    }

    /// A volume that refuses every hardlink (the exFAT / network-share
    /// shape niubash#464 is about).
    fn always_fail_link(_source: &Path, _link: &Path) -> std::io::Result<()> {
        Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "hardlinks are not available on this volume",
        ))
    }

    #[test]
    fn faces_fall_back_to_shim_copies_where_hardlinks_fail() {
        let (root, niu) = fake_install_with_payload("shim-fallback");

        let report = link_shell_faces_with_linker(&root, &niu, false, &always_fail_link);
        assert_eq!(report.shimmed.len(), 6, "{report:?}");
        assert!(report.created.is_empty(), "{report:?}");
        assert!(report.failed.is_empty(), "{report:?}");
        for link in planned_shell_links(&root, &niu) {
            assert_eq!(std::fs::read(&link).unwrap(), b"shim payload");
        }

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn faces_fail_honestly_where_links_fail_and_no_payload_is_staged() {
        let (root, niu) = fake_install("shim-no-payload");

        let report = link_shell_faces_with_linker(&root, &niu, false, &always_fail_link);
        assert_eq!(report.failed.len(), 6, "{report:?}");
        assert!(report.shimmed.is_empty(), "{report:?}");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn existing_shim_faces_stay_shimmed_where_links_fail() {
        let (root, niu) = fake_install_with_payload("shim-stays");
        for link in planned_shell_links(&root, &niu) {
            std::fs::write(&link, b"shim payload").unwrap();
        }

        let report = link_shell_faces_with_linker(&root, &niu, false, &always_fail_link);
        assert_eq!(report.shimmed.len(), 6, "{report:?}");
        assert!(report.failed.is_empty(), "{report:?}");
        for link in planned_shell_links(&root, &niu) {
            assert_eq!(std::fs::read(&link).unwrap(), b"shim payload");
        }

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn existing_shim_faces_upgrade_to_hardlinks_where_links_work() {
        let (root, niu) = fake_install_with_payload("shim-upgrade");
        for link in planned_shell_links(&root, &niu) {
            std::fs::write(&link, b"shim payload").unwrap();
        }

        let report = link_shell_faces_with_linker(&root, &niu, false, &test_link);
        assert_eq!(report.created.len(), 6, "{report:?}");
        assert!(report.shimmed.is_empty(), "{report:?}");
        for link in planned_shell_links(&root, &niu) {
            assert!(
                same_file(&link, &niu),
                "{} is not the shell",
                link.display()
            );
        }

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_directory_face_is_refused_not_replaced() {
        let (root, niu) = fake_install("directory-face");
        let face = root.join("bin").join(SHELL_FACES[0]);
        std::fs::create_dir_all(&face).unwrap();

        let report = link_shell_faces_with_linker(&root, &niu, true, &test_link);
        assert!(face.is_dir(), "the directory face must survive relink");
        assert_eq!(report.failed.len(), 1, "{report:?}");
        assert!(report.failed[0].1.contains("directory"), "{report:?}");

        let _ = std::fs::remove_dir_all(&root);
    }
}
