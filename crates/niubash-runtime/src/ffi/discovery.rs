//! DLL discovery (DESIGN §3.2), with the 2026 CWD-relative search
//! as the named anti-pattern this module exists to negate:
//!
//! 1. `NIU_WINUXCMD_FFI_DLL` override — absolute paths only
//!    (config rejects relatives before they reach here).
//! 2. `<root>/usr/bin/ffi/current.txt` — a one-line pointer naming
//!    the active version directory — under the installation root
//!    niu's existing winuxcmd discovery already selected
//!    (`winuxcmd::installation_root`), which covers both the wpm
//!    install and the bundled layout: both keep the same
//!    `<root>/usr/bin/...` shape.
//!
//! Never the current directory; never a PATH search for the DLL.
//! No hash, signature, or integrity verification happens here (or
//! anywhere in this module tree): the trust red line (2026-10-28)
//! forbids load-time re-verification. The only gate before a call
//! is the ABI version check in the provider.

use std::path::{Path, PathBuf};

/// A resolved DLL candidate: everything the provider needs to
/// load and to describe the load in the status face.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DllCandidate {
    pub dll_path: PathBuf,
    pub tiers_path: PathBuf,
    /// The version directory name the pointer selected
    /// (e.g. `v1.2.0`); for the override form, the DLL's own
    /// directory name.
    pub version_dir: String,
    /// Explicit install root handed to `winux_ffi_init`: the
    /// winuxcmd installation root the DLL's tree belongs to.
    pub install_root: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiscoveryError {
    /// No winuxcmd installation root is known (discovery of the
    /// exe itself failed upstream, or was never run).
    NoInstallRoot,
    /// `usr/bin/ffi/current.txt` does not exist under the root.
    PointerMissing(PathBuf),
    /// The pointer file is empty or names something that is not a
    /// plain directory name.
    PointerMalformed(PathBuf),
    /// The version directory the pointer names does not exist.
    VersionDirMissing(PathBuf),
    /// The candidate directory has no `winuxcore.dll`.
    DllMissing(PathBuf),
    /// The candidate directory has no `tiers.tsv`.
    TiersMissing(PathBuf),
    /// The override path is not absolute (config normally filters
    /// this; kept as a hard check at the boundary).
    OverrideNotAbsolute(PathBuf),
    /// The override path does not name an existing file.
    OverrideMissing(PathBuf),
}

impl DiscoveryError {
    /// One-line, user-facing reason (status face / doctor). Every
    /// variant is a silent-fallback reason at dispatch time.
    pub fn describe(&self) -> String {
        match self {
            DiscoveryError::NoInstallRoot => "no winuxcmd installation root is known".to_string(),
            DiscoveryError::PointerMissing(path) => {
                format!("no FFI version pointer at {}", path.display())
            }
            DiscoveryError::PointerMalformed(path) => {
                format!("FFI version pointer at {} is malformed", path.display())
            }
            DiscoveryError::VersionDirMissing(path) => {
                format!("FFI version directory {} does not exist", path.display())
            }
            DiscoveryError::DllMissing(path) => {
                format!("winuxcore.dll missing at {}", path.display())
            }
            DiscoveryError::TiersMissing(path) => {
                format!("tiers.tsv missing at {}", path.display())
            }
            DiscoveryError::OverrideNotAbsolute(path) => format!(
                "NIU_WINUXCMD_FFI_DLL override {} is not absolute; ignored",
                path.display()
            ),
            DiscoveryError::OverrideMissing(path) => format!(
                "NIU_WINUXCMD_FFI_DLL override {} does not exist",
                path.display()
            ),
        }
    }
}

/// Resolve the DLL candidate.
///
/// `install_root` is the root of the winuxcmd installation niu
/// selected for this session (`winuxcmd::installation_root` of the
/// resolved exe); the FFI payload lives under it. The override, when
/// present, wins outright and derives its sibling `tiers.tsv` and
/// install root from its own location: the versioned layout is
/// `<root>/usr/bin/ffi/<version>/winuxcore.dll`, so the root is
/// four parents up; a DLL outside that shape keeps its own
/// directory as the root (development builds point straight at a
/// build tree).
pub fn resolve_candidate(
    dll_override: Option<&Path>,
    install_root: Option<&Path>,
) -> Result<DllCandidate, DiscoveryError> {
    if let Some(path) = dll_override {
        return resolve_override(path, install_root);
    }
    let root = install_root.ok_or(DiscoveryError::NoInstallRoot)?;
    let ffi_dir = root.join("usr").join("bin").join("ffi");
    let pointer_path = ffi_dir.join("current.txt");
    let raw = std::fs::read_to_string(&pointer_path)
        .map_err(|_| DiscoveryError::PointerMissing(pointer_path.clone()))?;
    let version_dir = raw.trim();
    if version_dir.is_empty()
        || version_dir.contains(['/', '\\'])
        || version_dir == "."
        || version_dir == ".."
    {
        return Err(DiscoveryError::PointerMalformed(pointer_path));
    }
    let dir = ffi_dir.join(version_dir);
    if !dir.is_dir() {
        return Err(DiscoveryError::VersionDirMissing(dir));
    }
    let dll_path = dir.join("winuxcore.dll");
    if !dll_path.is_file() {
        return Err(DiscoveryError::DllMissing(dll_path));
    }
    let tiers_path = dir.join("tiers.tsv");
    if !tiers_path.is_file() {
        return Err(DiscoveryError::TiersMissing(tiers_path));
    }
    Ok(DllCandidate {
        dll_path,
        tiers_path,
        version_dir: version_dir.to_string(),
        install_root: root.to_path_buf(),
    })
}

fn resolve_override(
    path: &Path,
    install_root: Option<&Path>,
) -> Result<DllCandidate, DiscoveryError> {
    if !path.is_absolute() {
        return Err(DiscoveryError::OverrideNotAbsolute(path.to_path_buf()));
    }
    if !path.is_file() {
        return Err(DiscoveryError::OverrideMissing(path.to_path_buf()));
    }
    let dir = path.parent().unwrap_or(path);
    let tiers_path = dir.join("tiers.tsv");
    if !tiers_path.is_file() {
        return Err(DiscoveryError::TiersMissing(tiers_path));
    }
    // Prefer the caller's known installation root; else derive it
    // from the versioned layout, else the DLL's own directory.
    let derived_root = dir
        .ancestors()
        .nth(4)
        .filter(|_| {
            dir.file_name().is_some()
                && dir
                    .parent()
                    .and_then(Path::file_name)
                    .is_some_and(|n| n == "ffi")
        })
        .map(Path::to_path_buf);
    let install_root = install_root
        .map(Path::to_path_buf)
        .or(derived_root)
        .unwrap_or_else(|| dir.to_path_buf());
    Ok(DllCandidate {
        dll_path: path.to_path_buf(),
        tiers_path,
        version_dir: dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
        install_root,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    static COUNTER: AtomicU32 = AtomicU32::new(0);

    fn temp_tree() -> PathBuf {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir =
            std::env::temp_dir().join(format!("niu-ffi-discovery-test-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create temp tree");
        dir
    }

    fn write(path: &Path, contents: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).expect("mkdir");
        std::fs::write(path, contents).expect("write");
    }

    #[test]
    fn pointer_form_resolves_version_directory() {
        let root = temp_tree();
        let version_dir = root.join("usr/bin/ffi/v9.9.9");
        write(&root.join("usr/bin/ffi/current.txt"), "v9.9.9\n");
        write(&version_dir.join("winuxcore.dll"), "dll");
        write(&version_dir.join("tiers.tsv"), "command\ttier\n");

        let candidate = resolve_candidate(None, Some(&root)).expect("resolves");
        assert_eq!(candidate.version_dir, "v9.9.9");
        assert_eq!(candidate.dll_path, version_dir.join("winuxcore.dll"));
        assert_eq!(candidate.tiers_path, version_dir.join("tiers.tsv"));
        assert_eq!(candidate.install_root, root);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn missing_pointer_and_malformed_pointer_are_distinct() {
        let root = temp_tree();
        assert!(matches!(
            resolve_candidate(None, Some(&root)),
            Err(DiscoveryError::PointerMissing(_))
        ));
        write(&root.join("usr/bin/ffi/current.txt"), "../escape\n");
        assert!(matches!(
            resolve_candidate(None, Some(&root)),
            Err(DiscoveryError::PointerMalformed(_))
        ));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn missing_dll_and_tiers_are_distinct() {
        let root = temp_tree();
        write(&root.join("usr/bin/ffi/current.txt"), "v1");
        let dir = root.join("usr/bin/ffi/v1");
        std::fs::create_dir_all(&dir).expect("mkdir");
        assert!(matches!(
            resolve_candidate(None, Some(&root)),
            Err(DiscoveryError::DllMissing(_))
        ));
        write(&dir.join("winuxcore.dll"), "dll");
        assert!(matches!(
            resolve_candidate(None, Some(&root)),
            Err(DiscoveryError::TiersMissing(_))
        ));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn override_wins_and_keeps_sibling_tiers() {
        let root = temp_tree();
        let dll = root.join("build/winuxcore.dll");
        write(&dll, "dll");
        write(&root.join("build/tiers.tsv"), "command\ttier\n");
        let candidate = resolve_candidate(Some(&dll), None).expect("resolves");
        assert_eq!(candidate.dll_path, dll);
        assert_eq!(candidate.tiers_path, root.join("build/tiers.tsv"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn no_root_and_relative_override() {
        assert_eq!(
            resolve_candidate(None, None),
            Err(DiscoveryError::NoInstallRoot)
        );
        assert!(matches!(
            resolve_candidate(Some(Path::new("relative/winuxcore.dll")), None),
            Err(DiscoveryError::OverrideNotAbsolute(_))
        ));
    }
}
