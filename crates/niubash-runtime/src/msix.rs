//! MSIX (Microsoft Store) packaging support (niubash#495).
//!
//! An MSIX package installs under `C:\Program Files\WindowsApps` — read-only
//! to the app itself. Everything Niubash materializes at runtime (the shell
//! root's `bin`/`usr`/`etc` tree, the winuxcmd applet link family, `.wpm`
//! state) must therefore land in the package's private writable area:
//!
//!   `%LOCALAPPDATA%\Packages\<PackageFamilyName>\LocalCache\niubash-root`
//!
//! [`apply_writable_shell_root`] points `NIU_ROOT` there before startup
//! prepares the shell root, and the winuxcmd activation path roots the
//! session's link family plus PATH injection under the same directory.
//! Per-user, per-package, and removed on uninstall — the only location
//! that satisfies all three.

use std::path::{Path, PathBuf};

/// The shell-root directory created under the package's `LocalCache`.
#[cfg(windows)]
const WRITABLE_ROOT_DIR: &str = "niubash-root";

/// The process's MSIX package family name, or `None` when it is not
/// running packaged (`GetCurrentPackageFamilyName` answers
/// `APPMODEL_ERROR_NO_PACKAGE`). Cached: a process's package identity
/// cannot change over its lifetime.
///
/// `NIU_MSIX_TEST_FAMILY` fakes the identity for development machines that
/// cannot register an MSIX (no sideload rights): every downstream path —
/// writable-root redirection, link family, PATH injection, self-update
/// and doctor gates — then runs exactly as under a real package.
pub fn package_family_name() -> Option<String> {
    #[cfg(windows)]
    {
        static FAMILY: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
        FAMILY.get_or_init(query_package_family_name).clone()
    }
    #[cfg(not(windows))]
    {
        None
    }
}

#[cfg(windows)]
fn query_package_family_name() -> Option<String> {
    if let Some(fake) = std::env::var_os("NIU_MSIX_TEST_FAMILY")
        .filter(|value| !value.is_empty())
        .map(|value| value.to_string_lossy().into_owned())
    {
        return Some(fake);
    }
    query_package_family_name_win32()
}

#[cfg(windows)]
fn query_package_family_name_win32() -> Option<String> {
    use windows_sys::Win32::Foundation::ERROR_INSUFFICIENT_BUFFER;
    use windows_sys::Win32::Storage::Packaging::Appx::GetCurrentPackageFamilyName;

    let mut length = 0_u32;
    // SAFETY: nullptr buffer is the documented sizing call; `length` is a
    // live out-parameter. ERROR_INSUFFICIENT_BUFFER means a package
    // identity exists and sized the buffer; APPMODEL_ERROR_NO_PACKAGE
    // (15700) and anything else mean unpackaged.
    let rc = unsafe { GetCurrentPackageFamilyName(&mut length, std::ptr::null_mut()) };
    if rc != ERROR_INSUFFICIENT_BUFFER || length == 0 {
        return None;
    }
    let mut buffer = vec![0_u16; length as usize];
    // SAFETY: `buffer` is `length` UTF-16 code units of writable memory,
    // matching the contract the sizing call just established.
    let rc = unsafe { GetCurrentPackageFamilyName(&mut length, buffer.as_mut_ptr()) };
    if rc != 0 {
        return None;
    }
    while buffer.last() == Some(&0) {
        buffer.pop();
    }
    String::from_utf16(&buffer).ok()
}

/// Whether the current process runs with an MSIX package identity — i.e.
/// from a Microsoft Store or sideloaded MSIX install.
pub fn is_packaged() -> bool {
    package_family_name().is_some()
}

/// The user's real `%LOCALAPPDATA%`. Environment variables are not
/// virtualized for packaged processes, so this is the stable base under
/// which the `Packages\<family>` private area lives.
#[cfg(windows)]
fn local_app_data() -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

/// `<LOCALAPPDATA>\Packages\<family>\LocalCache\niubash-root`.
#[cfg(windows)]
fn writable_root_in(local_app_data: &Path, family: &str) -> PathBuf {
    local_app_data
        .join("Packages")
        .join(family)
        .join("LocalCache")
        .join(WRITABLE_ROOT_DIR)
}

/// This package's writable root under its private `LocalCache` — the only
/// location a packaged process may write that is per-user, per-package,
/// and cleaned up on uninstall. `None` when not packaged.
pub fn writable_root() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        Some(writable_root_in(
            &local_app_data()?,
            &package_family_name()?,
        ))
    }
    #[cfg(not(windows))]
    {
        None
    }
}

/// `NIU_ROOT` already present in the environment.
fn configured_shell_root() -> Option<PathBuf> {
    std::env::var_os("NIU_ROOT")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

/// The root this session's link family and PATH injection use under MSIX:
/// the configured shell root when one was given, else the package's own
/// `LocalCache` root. `None` when not packaged.
pub fn session_writable_root() -> Option<PathBuf> {
    if is_packaged() {
        configured_shell_root().or_else(writable_root)
    } else {
        None
    }
}

/// Point `NIU_ROOT` at the writable per-package root before startup
/// prepares the shell root (niubash#495: the package root is read-only, so
/// the default resolution fails every session with os error 5). An
/// explicit `NIU_ROOT` always wins — that override is the
/// escape hatch for layouts that already manage a root.
pub fn apply_writable_shell_root() {
    if !is_packaged() || configured_shell_root().is_some() {
        return;
    }
    if let Some(root) = writable_root() {
        std::env::set_var("NIU_ROOT", root);
    }
}

/// Whether `path` sits under the AppExecutionAlias stub directory
/// (`%LOCALAPPDATA%\Microsoft\WindowsApps`). Alias stubs are reparse
/// points serviced by the app-execution redirector, so they never
/// canonicalize to the package's real exe — path-equality checks (the
/// doctor's niu-on-PATH row) must treat them as the same origin
/// (niubash#495).
pub fn is_app_execution_alias_path(path: &Path) -> bool {
    #[cfg(windows)]
    {
        let Some(root) = local_app_data().map(|dir| dir.join("Microsoft").join("WindowsApps"))
        else {
            return false;
        };
        path.starts_with(root)
    }
    #[cfg(not(windows))]
    {
        let _ = path;
        false
    }
}

/// Create `link` as a directory junction to `target` — a mount-point
/// reparse point, which needs no elevation and works inside the package's
/// `LocalCache`. This is how the writable root's `opt` becomes a working
/// view over the read-only package payloads: winuxcmd's shim forwarder
/// resolves `<root>\opt\<pkg>` relative to the invoked link's own path, so
/// the link root must see the payloads (niubash#495).
#[cfg(windows)]
pub fn create_junction(link: &Path, target: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::{CloseHandle, GENERIC_WRITE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_DELETE,
        FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
    };
    use windows_sys::Win32::System::Ioctl::FSCTL_SET_REPARSE_POINT;
    use windows_sys::Win32::System::IO::DeviceIoControl;

    let target = std::fs::canonicalize(target)?;
    if !target.is_dir() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "junction target is not a directory",
        ));
    }
    std::fs::create_dir_all(link)?;

    // The substitute name is an NT path (\??\C:\...); canonicalize yields
    // the \\?\C:\... verbatim form. The print name is the plain absolute
    // path users see.
    let canonical = target.to_string_lossy().replace('/', "\\");
    let absolute = canonical
        .strip_prefix("\\\\?\\")
        .unwrap_or(&canonical)
        .to_string();
    let substitute = format!("\\??\\{absolute}");
    let substitute_wide: Vec<u16> = substitute.encode_utf16().collect();
    let print_wide: Vec<u16> = absolute.encode_utf16().collect();
    let buffer = mount_point_reparse_buffer(&substitute_wide, &print_wide);

    let link_wide: Vec<u16> = link
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    // SAFETY: `link_wide` is NUL-terminated and outlives the call; the
    // returned handle is checked and closed on every path.
    let handle = unsafe {
        CreateFileW(
            link_wide.as_ptr(),
            GENERIC_WRITE,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
            std::ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(std::io::Error::last_os_error());
    }
    let mut returned = 0_u32;
    // SAFETY: `handle` is a live directory handle opened with
    // GENERIC_WRITE; `buffer` is a valid mount-point REPARSE_DATA_BUFFER
    // of `buffer.len()` bytes that outlives the call.
    let ok = unsafe {
        DeviceIoControl(
            handle,
            FSCTL_SET_REPARSE_POINT,
            buffer.as_ptr().cast(),
            buffer.len() as u32,
            std::ptr::null_mut(),
            0,
            &mut returned,
            std::ptr::null_mut(),
        )
    };
    // SAFETY: `handle` is open.
    unsafe { CloseHandle(handle) };
    if ok == 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

/// Serialize a mount-point `REPARSE_DATA_BUFFER` by hand — windows-sys
/// exposes the ioctl and the tag, not the struct layout:
///   u32 tag | u16 data_len | u16 reserved
///   u16 subst_off | u16 subst_len | u16 print_off | u16 print_len
///   wchar path_buffer[] = subst + NUL + print + NUL
#[cfg(windows)]
fn mount_point_reparse_buffer(substitute: &[u16], print: &[u16]) -> Vec<u8> {
    use windows_sys::Win32::System::SystemServices::IO_REPARSE_TAG_MOUNT_POINT;

    let mut bytes = Vec::with_capacity(16 + (substitute.len() + print.len() + 2) * 2);
    let data_len = (8 + (substitute.len() + print.len() + 2) * 2) as u16;
    bytes.extend_from_slice(&IO_REPARSE_TAG_MOUNT_POINT.to_le_bytes());
    bytes.extend_from_slice(&data_len.to_le_bytes());
    bytes.extend_from_slice(&0_u16.to_le_bytes()); // reserved
    bytes.extend_from_slice(&0_u16.to_le_bytes()); // substitute offset
    bytes.extend_from_slice(&((substitute.len() * 2) as u16).to_le_bytes());
    bytes.extend_from_slice(&((substitute.len() + 1) as u16 * 2).to_le_bytes()); // print offset
    bytes.extend_from_slice(&((print.len() * 2) as u16).to_le_bytes());
    for ch in substitute {
        bytes.extend_from_slice(&ch.to_le_bytes());
    }
    bytes.extend_from_slice(&0_u16.to_le_bytes());
    for ch in print {
        bytes.extend_from_slice(&ch.to_le_bytes());
    }
    bytes.extend_from_slice(&0_u16.to_le_bytes());
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Layout of the writable root must match the location packaged
    /// sessions were verified against: LocalCache of the private package
    /// data area, not a guessed sibling.
    #[cfg(windows)]
    #[test]
    fn writable_root_lands_under_localcache() {
        let local = Path::new(r"C:\Users\me\AppData\Local");
        assert_eq!(
            writable_root_in(local, "NiubashTest_abc123"),
            PathBuf::from(
                r"C:\Users\me\AppData\Local\Packages\NiubashTest_abc123\LocalCache\niubash-root"
            )
        );
    }

    /// Non-packaged sessions never get a session root — the Inno/zip path
    /// must stay on the install tree it always used (zero regression).
    #[cfg(not(windows))]
    #[test]
    fn unpackaged_sessions_have_no_writable_root() {
        assert!(!is_packaged());
        assert!(writable_root().is_none());
        assert!(session_writable_root().is_none());
    }

    /// The mount-point reparse buffer is hand-built; pin its header fields
    /// and string placement so a layout slip cannot ship.
    #[cfg(windows)]
    #[test]
    fn reparse_buffer_matches_mount_point_layout() {
        let subst: Vec<u16> = "\\??\\C:\\a".encode_utf16().collect();
        let print: Vec<u16> = "C:\\a".encode_utf16().collect();
        let buf = mount_point_reparse_buffer(&subst, &print);

        let u32_at = |off: usize| u32::from_le_bytes(buf[off..off + 4].try_into().unwrap());
        let u16_at = |off: usize| u16::from_le_bytes(buf[off..off + 2].try_into().unwrap());

        assert_eq!(u32_at(0), 0xA0000003); // IO_REPARSE_TAG_MOUNT_POINT
        assert_eq!(u16_at(4) as usize, 8 + (subst.len() + print.len() + 2) * 2);
        assert_eq!(u16_at(6), 0); // reserved
        assert_eq!(u16_at(8), 0); // substitute offset
        assert_eq!(u16_at(10) as usize, subst.len() * 2);
        assert_eq!(u16_at(12) as usize, (subst.len() + 1) * 2); // print offset
        assert_eq!(u16_at(14) as usize, print.len() * 2);
        // Strings land back-to-back, each NUL-terminated.
        let mut expected = Vec::new();
        for ch in &subst {
            expected.extend_from_slice(&ch.to_le_bytes());
        }
        assert_eq!(&buf[16..16 + subst.len() * 2], expected.as_slice());
    }

    /// A junction must actually redirect directory traversal: read a file
    /// through the linked name and prove the reparse point exists.
    #[cfg(windows)]
    #[test]
    fn junction_redirects_directory_reads() {
        let base = std::env::temp_dir().join(format!(
            "niubash-msix-junction-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or_default()
        ));
        let target = base.join("target");
        let link = base.join("link");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(target.join("inner.txt"), b"payload").unwrap();

        create_junction(&link, &target).unwrap();

        use std::os::windows::fs::MetadataExt;
        assert!(link.join("inner.txt").is_file());
        // FILE_ATTRIBUTE_REPARSE_POINT (0x400) marks the mount-point
        // junction the ioctl just installed.
        assert_ne!(
            std::fs::symlink_metadata(&link).unwrap().file_attributes() & 0x400,
            0
        );

        let _ = std::fs::remove_dir_all(&link);
        let _ = std::fs::remove_dir_all(&base);
    }
}
