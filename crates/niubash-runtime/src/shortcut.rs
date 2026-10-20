//! Desktop / Start-menu shortcut generation (niubash#211).
//!
//! Win10 has no "default terminal application" setting, so a shortcut that
//! points straight at `niu.exe` is hosted by conhost — poor fonts, poor
//! colors, no Nerd Font glyphs. When Windows Terminal is present (and the
//! Niubash WT profile is registered, so `wt.exe -p Niubash` resolves), the
//! shortcuts are generated in the WT form instead; machines without WT keep
//! the conhost baseline unchanged.
//!
//! The `wt.exe` resolution follows a deliberately robust order (issue #211
//! point 3: the WindowsApps execution alias is shared between WT stable and
//! Preview, and the OS hands it to whichever install won — resolving the
//! alias file itself, rather than a package-specific path, sidesteps the
//! preemption question entirely):
//!
//! 1. the per-user WindowsApps execution alias
//!    (`%LOCALAPPDATA%\Microsoft\WindowsApps\wt.exe`),
//! 2. the process `PATH` (scoop / chocolatey / manual unpackaged installs),
//! 3. the `App Paths` registry key (`HKCU` then `HKLM`), which some
//!    unpackaged distributions register.
//!
//! The `.lnk` files are written through the shell link COM object. The
//! windows-sys 0.59 surface carries COM free functions but no interface
//! definitions, so [`IShellLinkWVtbl`] / [`IPersistFileVtbl`] below declare
//! the two vtables this module drives, in exact SDK declaration order
//! (shobjidl_core.h / objidl.h). The round-trip integration test
//! (`tests/wt_shortcut_roundtrip.rs`) writes and reads links back through
//! these vtables, so an ordering mistake fails loudly instead of corrupting
//! launches.

#[cfg(windows)]
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

/// The `wt.exe` arguments a WT-form shortcut carries: launch the Niubash
/// profile (font / color scheme / `startingDirectory: %USERPROFILE%` come
/// from the registered profile, not the shortcut).
pub const WT_SHORTCUT_ARGUMENTS: &str = "-p Niubash";

/// The link file name inside the desktop / Start-menu group directories.
/// Must match the installer's `[Icons]` entry (`Name: "...\Niubash"`), so a
/// post-install rewrite replaces the same file Inno logged for uninstall.
pub const LNK_FILE_NAME: &str = "Niubash.lnk";

/// Which launch form a generated shortcut takes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShortcutForm {
    /// `wt.exe -p Niubash` — routed through Windows Terminal so Win10
    /// sessions escape conhost.
    WindowsTerminal {
        /// The resolved `wt.exe` (alias, PATH hit, or registry entry).
        wt_exe: PathBuf,
    },
    /// Direct `niu.exe` — the conhost baseline for machines without a
    /// usable WT (or without the registered profile the `-p` form needs).
    Conhost,
}

/// Everything a `.lnk` needs, decided once and testable without COM.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShortcutPlan {
    pub form: ShortcutForm,
    pub target: PathBuf,
    pub arguments: String,
    /// `%USERPROFILE%` — never System32 (issue #211: a conhost-inherited
    /// CWD lands there when the parent process's directory leaks in).
    pub working_directory: PathBuf,
    pub icon: Option<PathBuf>,
}

impl ShortcutPlan {
    /// The niubash#211 shortcut branch: a resolved `wt.exe` produces the
    /// WT form; `None` (WT absent) keeps the conhost status quo — the
    /// direct-exe shortcut the installer has always created.
    pub fn build(
        wt_exe: Option<&Path>,
        niu_exe: &Path,
        icon: Option<&Path>,
        working_directory: &Path,
    ) -> ShortcutPlan {
        match wt_exe {
            Some(wt) => ShortcutPlan {
                form: ShortcutForm::WindowsTerminal {
                    wt_exe: wt.to_path_buf(),
                },
                target: wt.to_path_buf(),
                arguments: WT_SHORTCUT_ARGUMENTS.to_string(),
                working_directory: working_directory.to_path_buf(),
                icon: icon.map(Path::to_path_buf),
            },
            None => ShortcutPlan {
                form: ShortcutForm::Conhost,
                target: niu_exe.to_path_buf(),
                arguments: String::new(),
                working_directory: working_directory.to_path_buf(),
                icon: icon.map(Path::to_path_buf),
            },
        }
    }
}

/// Resolve `wt.exe` on this machine: WindowsApps alias → PATH → App Paths
/// registry (`HKCU`, then `HKLM`). `None` when none of the tiers yields an
/// existing file — the caller falls back to the conhost shortcut form.
pub fn resolve_wt_exe() -> Option<PathBuf> {
    let alias =
        dirs::data_local_dir().map(|dir| dir.join("Microsoft").join("WindowsApps").join("wt.exe"));
    let path_dirs = std::env::var_os("PATH")
        .map(|value| std::env::split_paths(&value).collect::<Vec<_>>())
        .unwrap_or_default();
    let registry = wt_app_paths_registry_candidates();
    resolve_wt_exe_from(alias.as_deref(), &path_dirs, &registry)
}

/// Testable form of [`resolve_wt_exe`]: the caller supplies the alias path
/// candidate, the PATH directories, and the registry-derived candidates;
/// this function owns the priority order.
pub fn resolve_wt_exe_from(
    alias: Option<&Path>,
    path_dirs: &[PathBuf],
    registry_candidates: &[PathBuf],
) -> Option<PathBuf> {
    if alias.is_some_and(|path| path.is_file()) {
        return alias.map(Path::to_path_buf);
    }
    if let Some(hit) = path_dirs
        .iter()
        .map(|dir| dir.join("wt.exe"))
        .find(|candidate| candidate.is_file())
    {
        return Some(hit);
    }
    registry_candidates
        .iter()
        .find(|candidate| candidate.is_file())
        .cloned()
}

/// Default-value candidates of the `App Paths\wt.exe` registry key, HKCU
/// before HKLM (a per-user install should win over a machine-wide one).
/// Empty on read failure — the registry is the last tier, not a hard
/// dependency.
#[cfg(windows)]
fn wt_app_paths_registry_candidates() -> Vec<PathBuf> {
    use windows_sys::Win32::Foundation::ERROR_SUCCESS;
    use windows_sys::Win32::System::Registry::{
        RegCloseKey, RegOpenKeyExW, RegQueryValueExW, HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE,
        KEY_READ, REG_EXPAND_SZ, REG_SZ,
    };

    const APP_PATHS_KEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\App Paths\\wt.exe";

    fn to_wide(value: &str) -> Vec<u16> {
        value.encode_utf16().chain(std::iter::once(0)).collect()
    }

    let mut candidates = Vec::new();
    for root in [HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE] {
        let mut key: HKEY = std::ptr::null_mut();
        let opened =
            unsafe { RegOpenKeyExW(root, to_wide(APP_PATHS_KEY).as_ptr(), 0, KEY_READ, &mut key) };
        if opened != ERROR_SUCCESS {
            continue;
        }
        let mut buffer = vec![0u16; 1024];
        let mut len = buffer.len() as u32;
        let mut kind = 0u32;
        let status = unsafe {
            RegQueryValueExW(
                key,
                std::ptr::null(),
                std::ptr::null_mut(),
                &mut kind,
                buffer.as_mut_ptr() as *mut u8,
                &mut len,
            )
        };
        unsafe {
            RegCloseKey(key);
        }
        if status != ERROR_SUCCESS || (kind != REG_SZ && kind != REG_EXPAND_SZ) {
            continue;
        }
        let value = wide_to_string(&buffer);
        if value.is_empty() {
            continue;
        }
        // REG_EXPAND_SZ values may carry %env% references.
        candidates.push(PathBuf::from(expand_env_strings(&value)));
    }
    candidates
}

#[cfg(windows)]
fn expand_env_strings(value: &str) -> String {
    // %VAR% expansion without a full shell: only WindowsApps-style
    // percentages appear in App Paths defaults in practice.
    let mut out = String::with_capacity(value.len());
    let mut rest = value;
    while let Some(start) = rest.find('%') {
        let after = &rest[start + 1..];
        match after.find('%') {
            Some(end) => {
                let name = &after[..end];
                out.push_str(&rest[..start]);
                match std::env::var(name) {
                    Ok(expanded) => out.push_str(&expanded),
                    Err(_) => {
                        // Unknown variable: keep the literal text.
                        out.push('%');
                        out.push_str(name);
                        out.push('%');
                    }
                }
                rest = &after[end + 1..];
            }
            None => break,
        }
    }
    out.push_str(rest);
    out
}

#[cfg(not(windows))]
fn wt_app_paths_registry_candidates() -> Vec<PathBuf> {
    Vec::new()
}

#[cfg(windows)]
fn wide_to_string(buffer: &[u16]) -> String {
    let end = buffer
        .iter()
        .position(|&unit| unit == 0)
        .unwrap_or(buffer.len());
    String::from_utf16_lossy(&buffer[..end])
}

/// Outcome of one shortcut install pass: which links were (re)written and
/// which were deliberately left alone.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ShortcutInstallSummary {
    pub written: Vec<PathBuf>,
    pub skipped: Vec<PathBuf>,
}

/// Write the desktop and Start-menu links per `plan`.
///
/// `update_only` exists for the installer handoff: it touches only links
/// that already exist (Inno created exactly the icons the user opted into),
/// so the rewrite never grows a desktop icon the desktopicon task declined.
/// A bare run (no installer baseline) passes `false` and creates both.
#[cfg(windows)]
pub fn install_shortcuts(
    plan: &ShortcutPlan,
    desktop_lnk: &Path,
    start_menu_lnk: &Path,
    update_only: bool,
) -> Result<ShortcutInstallSummary> {
    let mut summary = ShortcutInstallSummary::default();
    for lnk in [desktop_lnk, start_menu_lnk] {
        if update_only && !lnk.is_file() {
            summary.skipped.push(lnk.to_path_buf());
            continue;
        }
        if let Some(parent) = lnk.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create {}", parent.display()))?;
        }
        write_shortcut(plan, lnk).with_context(|| format!("write {}", lnk.display()))?;
        summary.written.push(lnk.to_path_buf());
    }
    Ok(summary)
}

/// The default link targets when niu runs outside the installer: the user's
/// desktop plus the `Niubash` Start-menu group (created if missing), both
/// named [`LNK_FILE_NAME`] to line up with the installer's own entries.
#[cfg(windows)]
pub fn default_shortcut_paths() -> Option<(PathBuf, PathBuf)> {
    let desktop = known_folder(&FOLDERID_DESKTOP)?.join(LNK_FILE_NAME);
    let group = known_folder(&FOLDERID_PROGRAMS)?.join("Niubash");
    std::fs::create_dir_all(&group).ok()?;
    Some((desktop, group.join(LNK_FILE_NAME)))
}

#[cfg(windows)]
pub fn plan_for_installation(niu_exe: &Path, icon: Option<&Path>) -> ShortcutPlan {
    let working_directory = dirs::home_dir().unwrap_or_else(|| {
        niu_exe
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."))
    });
    let resolved = resolve_wt_exe();
    let wt_exe = resolved.as_deref().filter(|exe| {
        wt_form_eligible(
            Some(exe),
            super::windows_terminal::niubash_profile_registered(),
        )
    });
    ShortcutPlan::build(wt_exe.as_deref(), niu_exe, icon, &working_directory)
}

/// The niubash#211 eligibility rule in one place: the WT shortcut form
/// requires BOTH a resolvable `wt.exe` AND a registered Niubash profile —
/// `wt -p` against an unknown profile name errors out, so a machine whose
/// profile task was skipped (or that has no WT at all) keeps launching
/// through the conhost baseline.
pub fn wt_form_eligible(wt_exe: Option<&Path>, profile_registered: bool) -> bool {
    wt_exe.is_some() && profile_registered
}

#[cfg(windows)]
use windows_sys::core::{GUID, HRESULT, PCWSTR, PWSTR};

#[cfg(windows)]
const FOLDERID_DESKTOP: GUID = GUID::from_u128(0xb4bfcc3a_db2c_424c_b029_7fe99a87c641); // FOLDERID_Desktop
#[cfg(windows)]
const FOLDERID_PROGRAMS: GUID = GUID::from_u128(0xa77f5d77_2e2b_44c3_a6a2_aba601054a51); // FOLDERID_Programs
#[cfg(windows)]
const IID_ISHELLLINKW: GUID = GUID::from_u128(0x000214f9_0000_0000_c000_000000000046);
#[cfg(windows)]
const IID_IPERSISTFILE: GUID = GUID::from_u128(0x0000010b_0000_0000_c000_000000000046);

// ── Raw COM plumbing ────────────────────────────────────────────────────────
//
// windows-sys 0.59 exposes the COM free functions (CoCreateInstance,
// CoInitializeEx, ...) but not interface definitions, so the two vtables the
// shell link path needs are declared here in exact SDK order. Method ORDER
// IS ABI: adding, removing, or reordering a field silently redirects every
// later call — the round-trip test pins the whole set.

#[cfg(windows)]
type IUnknownMethodSelf = *mut std::ffi::c_void;

#[cfg(windows)]
#[repr(C)]
#[allow(non_snake_case)] // COM method names are the ABI's own convention
struct IUnknownVtbl {
    QueryInterface: unsafe extern "system" fn(
        IUnknownMethodSelf,
        *const GUID,
        *mut *mut std::ffi::c_void,
    ) -> HRESULT,
    AddRef: unsafe extern "system" fn(IUnknownMethodSelf) -> u32,
    Release: unsafe extern "system" fn(IUnknownMethodSelf) -> u32,
}

/// `IShellLinkW` vtable, in exact SDK declaration order (shobjidl_core.h;
/// note `SetPath` is the interface's LAST method, after `Resolve` — the
/// common interop listings that show it second are wrong, and that slot
/// silently hits `GetIDList` instead):
/// GetPath, GetIDList, SetIDList, GetDescription, SetDescription,
/// GetWorkingDirectory, SetWorkingDirectory, GetArguments, SetArguments,
/// GetHotkey, SetHotkey, GetShowCmd, SetShowCmd, GetIconLocation,
/// SetIconLocation, SetRelativePath, Resolve, SetPath.
#[cfg(windows)]
#[repr(C)]
#[allow(non_snake_case)] // COM method names are the ABI's own convention
struct IShellLinkWVtbl {
    base: IUnknownVtbl,
    GetPath: unsafe extern "system" fn(
        IUnknownMethodSelf,
        PWSTR,
        i32,
        *mut std::ffi::c_void, // WIN32_FIND_DATAW*
        u32,
    ) -> HRESULT,
    GetIDList: unsafe extern "system" fn(IUnknownMethodSelf, *mut *mut std::ffi::c_void) -> HRESULT,
    SetIDList: unsafe extern "system" fn(IUnknownMethodSelf, *const std::ffi::c_void) -> HRESULT,
    GetDescription: unsafe extern "system" fn(IUnknownMethodSelf, PWSTR, i32) -> HRESULT,
    SetDescription: unsafe extern "system" fn(IUnknownMethodSelf, PCWSTR) -> HRESULT,
    GetWorkingDirectory: unsafe extern "system" fn(IUnknownMethodSelf, PWSTR, i32) -> HRESULT,
    SetWorkingDirectory: unsafe extern "system" fn(IUnknownMethodSelf, PCWSTR) -> HRESULT,
    GetArguments: unsafe extern "system" fn(IUnknownMethodSelf, PWSTR, i32) -> HRESULT,
    SetArguments: unsafe extern "system" fn(IUnknownMethodSelf, PCWSTR) -> HRESULT,
    GetHotkey: unsafe extern "system" fn(IUnknownMethodSelf, *mut u16) -> HRESULT,
    SetHotkey: unsafe extern "system" fn(IUnknownMethodSelf, u16) -> HRESULT,
    GetShowCmd: unsafe extern "system" fn(IUnknownMethodSelf, *mut i32) -> HRESULT,
    SetShowCmd: unsafe extern "system" fn(IUnknownMethodSelf, i32) -> HRESULT,
    GetIconLocation: unsafe extern "system" fn(IUnknownMethodSelf, PWSTR, i32, *mut i32) -> HRESULT,
    SetIconLocation: unsafe extern "system" fn(IUnknownMethodSelf, PCWSTR, i32) -> HRESULT,
    SetRelativePath: unsafe extern "system" fn(IUnknownMethodSelf, PCWSTR, u32) -> HRESULT,
    Resolve: unsafe extern "system" fn(
        IUnknownMethodSelf,
        windows_sys::Win32::Foundation::HWND,
        u32,
    ) -> HRESULT,
    SetPath: unsafe extern "system" fn(IUnknownMethodSelf, PCWSTR) -> HRESULT,
}

/// `IPersistFile` vtable, in objidl.h declaration order:
/// GetClassID (IPersist), IsDirty, Load, Save, SaveCompleted, GetCurFile.
#[cfg(windows)]
#[repr(C)]
#[allow(non_snake_case)] // COM method names are the ABI's own convention
struct IPersistFileVtbl {
    base: IUnknownVtbl,
    GetClassID: unsafe extern "system" fn(IUnknownMethodSelf, *mut GUID) -> HRESULT,
    IsDirty: unsafe extern "system" fn(IUnknownMethodSelf) -> HRESULT,
    Load: unsafe extern "system" fn(IUnknownMethodSelf, PCWSTR, u32) -> HRESULT,
    Save: unsafe extern "system" fn(IUnknownMethodSelf, PCWSTR, i32) -> HRESULT,
    SaveCompleted: unsafe extern "system" fn(IUnknownMethodSelf, PCWSTR) -> HRESULT,
    GetCurFile: unsafe extern "system" fn(IUnknownMethodSelf, *mut PWSTR) -> HRESULT,
}

/// RAII for CoInitializeEx: balances CoUninitialize only when this call
/// actually initialized the apartment (S_OK / S_FALSE). RPC_E_CHANGED_MODE
/// means COM is already up with a different concurrency model — usable as
///-is, and uninitializing it would break the host.
#[cfg(windows)]
struct ComApartment {
    uninitialize: bool,
}

#[cfg(windows)]
impl Drop for ComApartment {
    fn drop(&mut self) {
        if self.uninitialize {
            unsafe {
                windows_sys::Win32::System::Com::CoUninitialize();
            }
        }
    }
}

#[cfg(windows)]
fn com_apartment() -> Result<ComApartment> {
    use windows_sys::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};

    const RPC_E_CHANGED_MODE: HRESULT = 0x8001_0106u32 as HRESULT;
    const S_FALSE: HRESULT = 1;

    let hr = unsafe { CoInitializeEx(std::ptr::null(), COINIT_APARTMENTTHREADED as u32) };
    if hr == RPC_E_CHANGED_MODE {
        return Ok(ComApartment {
            uninitialize: false,
        });
    }
    if hr == 0 || hr == S_FALSE {
        return Ok(ComApartment { uninitialize: true });
    }
    Err(anyhow::anyhow!("CoInitializeEx failed: 0x{hr:08X}"))
}

#[cfg(windows)]
fn to_wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Write one `.lnk` file per `plan` through the shell link COM object.
/// The parent directory is created if missing (`IPersistFile::Save` does
/// not create it and answers ERROR_PATH_NOT_FOUND otherwise).
#[cfg(windows)]
pub fn write_shortcut(plan: &ShortcutPlan, lnk_path: &Path) -> Result<()> {
    use windows_sys::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};
    use windows_sys::Win32::UI::Shell::ShellLink;

    if let Some(parent) = lnk_path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }

    let _apartment = com_apartment()?;
    let mut link: *mut std::ffi::c_void = std::ptr::null_mut();
    let hr = unsafe {
        CoCreateInstance(
            &ShellLink,
            std::ptr::null_mut(),
            CLSCTX_INPROC_SERVER,
            &IID_ISHELLLINKW,
            &mut link,
        )
    };
    anyhow::ensure!(hr == 0, "CoCreateInstance(ShellLink) failed: 0x{hr:08X}");

    let result = write_shortcut_into(link, plan, lnk_path);
    unsafe {
        release(link);
    }
    result.with_context(|| format!("write shortcut {}", lnk_path.display()))
}

/// The shell-link half of the write: `link` is an initialized
/// `IShellLinkW`, already Co-created. Split out so error paths can still
/// Release the object exactly once.
#[cfg(windows)]
fn write_shortcut_into(
    link: *mut std::ffi::c_void,
    plan: &ShortcutPlan,
    lnk_path: &Path,
) -> Result<()> {
    let vtbl: *const IShellLinkWVtbl = unsafe { *(link as *mut *const IShellLinkWVtbl) };

    let target = to_wide(&plan.target.to_string_lossy());
    let arguments = to_wide(&plan.arguments);
    let working_directory = to_wide(&plan.working_directory.to_string_lossy());

    unsafe {
        let set_path = ((*vtbl).SetPath)(link, target.as_ptr());
        anyhow::ensure!(
            set_path == 0,
            "IShellLinkW::SetPath failed: 0x{set_path:08X}"
        );
        ((*vtbl).SetArguments)(link, arguments.as_ptr());
        ((*vtbl).SetWorkingDirectory)(link, working_directory.as_ptr());
        if let Some(icon) = &plan.icon {
            let icon = to_wide(&icon.to_string_lossy());
            ((*vtbl).SetIconLocation)(link, icon.as_ptr(), 0);
        }
    }

    // Persist: QI for IPersistFile, then Save.
    let mut persist: *mut std::ffi::c_void = std::ptr::null_mut();
    let hr = unsafe { ((*vtbl).base.QueryInterface)(link, &IID_IPERSISTFILE, &mut persist) };
    anyhow::ensure!(
        hr == 0,
        "IShellLinkW does not expose IPersistFile: 0x{hr:08X}"
    );

    let save = unsafe {
        let path = to_wide(&lnk_path.to_string_lossy());
        let saved =
            ((*(*(persist as *mut *const IPersistFileVtbl))).Save)(persist, path.as_ptr(), 1);
        release(persist);
        saved
    };
    anyhow::ensure!(save == 0, "IPersistFile::Save failed: 0x{save:08X}");
    Ok(())
}

#[cfg(windows)]
unsafe fn release(unknown: *mut std::ffi::c_void) {
    let vtbl: *const IUnknownVtbl = *(unknown as *mut *const IUnknownVtbl);
    ((*vtbl).Release)(unknown);
}

/// Everything read back from a `.lnk`, used to pin the write path in tests
/// (and later by `niu doctor` for shortcut diagnostics).
#[cfg(windows)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShortcutReadback {
    pub target: PathBuf,
    pub arguments: String,
    pub icon: Option<PathBuf>,
    pub working_directory: PathBuf,
}

/// Load a `.lnk` through the same shell link COM object and return the
/// fields [`write_shortcut`] set. Verification counterpart of the writer —
/// if either vtable drifted from the ABI, this round trip fails loudly.
#[cfg(windows)]
pub fn read_shortcut(lnk_path: &Path) -> Result<ShortcutReadback> {
    use windows_sys::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};
    use windows_sys::Win32::UI::Shell::ShellLink;

    let _apartment = com_apartment()?;
    let mut link: *mut std::ffi::c_void = std::ptr::null_mut();
    let hr = unsafe {
        CoCreateInstance(
            &ShellLink,
            std::ptr::null_mut(),
            CLSCTX_INPROC_SERVER,
            &IID_ISHELLLINKW,
            &mut link,
        )
    };
    anyhow::ensure!(hr == 0, "CoCreateInstance(ShellLink) failed: 0x{hr:08X}");

    let result = read_shortcut_from(link, lnk_path);
    unsafe {
        release(link);
    }
    result.with_context(|| format!("read shortcut {}", lnk_path.display()))
}

#[cfg(windows)]
fn read_shortcut_from(link: *mut std::ffi::c_void, lnk_path: &Path) -> Result<ShortcutReadback> {
    const BUF_CHARS: usize = 1024;

    let vtbl: *const IShellLinkWVtbl = unsafe { *(link as *mut *const IShellLinkWVtbl) };

    let mut persist: *mut std::ffi::c_void = std::ptr::null_mut();
    let hr = unsafe { ((*vtbl).base.QueryInterface)(link, &IID_IPERSISTFILE, &mut persist) };
    anyhow::ensure!(
        hr == 0,
        "IShellLinkW does not expose IPersistFile: 0x{hr:08X}"
    );
    let load = unsafe {
        let path = to_wide(&lnk_path.to_string_lossy());
        let loaded =
            ((*(*(persist as *mut *const IPersistFileVtbl))).Load)(persist, path.as_ptr(), 0);
        release(persist);
        loaded
    };
    anyhow::ensure!(load == 0, "IPersistFile::Load failed: 0x{load:08X}");

    unsafe {
        let mut target_buffer = vec![0u16; BUF_CHARS];
        let get_path = ((*vtbl).GetPath)(
            link,
            target_buffer.as_mut_ptr(),
            BUF_CHARS as i32,
            std::ptr::null_mut(),
            0,
        );
        anyhow::ensure!(
            get_path == 0,
            "IShellLinkW::GetPath failed: 0x{get_path:08X}"
        );

        let mut arguments_buffer = vec![0u16; BUF_CHARS];
        ((*vtbl).GetArguments)(link, arguments_buffer.as_mut_ptr(), BUF_CHARS as i32);

        let mut icon_buffer = vec![0u16; BUF_CHARS];
        let mut icon_index: i32 = 0;
        ((*vtbl).GetIconLocation)(
            link,
            icon_buffer.as_mut_ptr(),
            BUF_CHARS as i32,
            &mut icon_index,
        );

        let mut working_buffer = vec![0u16; BUF_CHARS];
        ((*vtbl).GetWorkingDirectory)(link, working_buffer.as_mut_ptr(), BUF_CHARS as i32);

        let icon_path = wide_to_string(&icon_buffer);
        Ok(ShortcutReadback {
            target: PathBuf::from(wide_to_string(&target_buffer)),
            arguments: wide_to_string(&arguments_buffer),
            icon: if icon_path.is_empty() {
                None
            } else {
                let mut icon = PathBuf::from(icon_path);
                if icon_index != 0 {
                    icon.set_extension(format!("ico,{}", icon_index));
                }
                Some(icon)
            },
            working_directory: PathBuf::from(wide_to_string(&working_buffer)),
        })
    }
}

#[cfg(windows)]
fn known_folder(folder: &GUID) -> Option<PathBuf> {
    use windows_sys::Win32::System::Com::CoTaskMemFree;
    use windows_sys::Win32::UI::Shell::SHGetKnownFolderPath;

    let mut path: PWSTR = std::ptr::null_mut();
    let hr = unsafe { SHGetKnownFolderPath(folder, 0, std::ptr::null_mut(), &mut path) };
    if hr != 0 || path.is_null() {
        if !path.is_null() {
            unsafe { CoTaskMemFree(path as *const std::ffi::c_void) };
        }
        return None;
    }
    unsafe {
        let mut len = 0usize;
        while *path.add(len) != 0 {
            len += 1;
        }
        let value = String::from_utf16_lossy(std::slice::from_raw_parts(path, len));
        CoTaskMemFree(path as *const std::ffi::c_void);
        Some(PathBuf::from(value))
    }
}

#[cfg(all(windows, test))]
mod tests {
    use super::*;
    use crate::windows_terminal::{NIU_PROFILE_GUID, NIU_PROFILE_NAME};
    use serde_json::json;

    fn staging(alias_dir: &Path, path_dir: &Path) -> (PathBuf, Vec<PathBuf>) {
        let alias = alias_dir
            .join("Microsoft")
            .join("WindowsApps")
            .join("wt.exe");
        std::fs::create_dir_all(alias.parent().unwrap()).unwrap();
        std::fs::write(&alias, b"alias").unwrap();
        let path_dirs = vec![path_dir.to_path_buf()];
        (alias, path_dirs)
    }

    #[test]
    fn arguments_address_the_registered_profile_name() {
        // The WT-form arguments must address the profile the installer
        // registers (windows_terminal::NIU_PROFILE_NAME) — a rename drift
        // between the two modules would break every generated shortcut.
        assert_eq!(WT_SHORTCUT_ARGUMENTS, format!("-p {NIU_PROFILE_NAME}"));
    }

    #[test]
    fn resolve_prefers_windowsapps_alias_over_path_and_registry() {
        let temp = tempfile::tempdir().unwrap();
        let (alias, path_dirs) = staging(temp.path(), &temp.path().join("bin"));
        let registry_alias = temp.path().join("registry").join("wt.exe");
        std::fs::create_dir_all(registry_alias.parent().unwrap()).unwrap();
        std::fs::write(&registry_alias, b"registry").unwrap();

        assert_eq!(
            resolve_wt_exe_from(
                Some(&alias),
                &path_dirs,
                std::slice::from_ref(&registry_alias)
            ),
            Some(alias)
        );
    }

    #[test]
    fn resolve_falls_back_to_path_then_registry() {
        let temp = tempfile::tempdir().unwrap();
        let path_dir = temp.path().join("bin");
        std::fs::create_dir_all(&path_dir).unwrap();
        std::fs::write(path_dir.join("wt.exe"), b"path").unwrap();
        let registry = temp.path().join("registry").join("wt.exe");
        std::fs::create_dir_all(registry.parent().unwrap()).unwrap();
        std::fs::write(&registry, b"registry").unwrap();
        let absent_alias = temp.path().join("none").join("wt.exe");

        // Alias missing: PATH wins over the registry tier.
        assert_eq!(
            resolve_wt_exe_from(
                Some(&absent_alias),
                std::slice::from_ref(&path_dir),
                std::slice::from_ref(&registry)
            ),
            Some(path_dir.join("wt.exe"))
        );
        // PATH missing too: the registry candidate is used.
        let empty = temp.path().join("empty-bin");
        std::fs::create_dir_all(&empty).unwrap();
        assert_eq!(
            resolve_wt_exe_from(
                Some(&absent_alias),
                &[empty],
                std::slice::from_ref(&registry)
            ),
            Some(registry)
        );
    }

    #[test]
    fn resolve_returns_none_when_all_tiers_missing() {
        let temp = tempfile::tempdir().unwrap();
        let empty = temp.path().join("empty");
        std::fs::create_dir_all(&empty).unwrap();
        assert_eq!(
            resolve_wt_exe_from(
                Some(&temp.path().join("none").join("wt.exe")),
                &[empty],
                &[temp.path().join("none2").join("wt.exe")]
            ),
            None
        );
    }

    #[test]
    fn plan_uses_wt_form_when_wt_resolved() {
        let wt = Path::new("C:/Users/me/AppData/Local/Microsoft/WindowsApps/wt.exe");
        let niu = Path::new("C:/Users/me/AppData/Local/Programs/Niubash/niu.exe");
        let ico = Path::new("C:/Users/me/AppData/Local/Programs/Niubash/assets/niubash-icon.ico");
        let home = Path::new("C:/Users/me");

        let plan = ShortcutPlan::build(Some(wt), niu, Some(ico), home);

        assert_eq!(
            plan.form,
            ShortcutForm::WindowsTerminal {
                wt_exe: wt.to_path_buf()
            }
        );
        assert_eq!(plan.target, wt);
        assert_eq!(plan.arguments, WT_SHORTCUT_ARGUMENTS);
        assert_eq!(plan.working_directory, home);
        assert_eq!(plan.icon, Some(ico.to_path_buf()));
    }

    #[test]
    fn plan_keeps_conhost_form_without_wt() {
        let niu = Path::new("C:/Users/me/AppData/Local/Programs/Niubash/niu.exe");
        let home = Path::new("C:/Users/me");

        let plan = ShortcutPlan::build(None, niu, None, home);

        assert_eq!(plan.form, ShortcutForm::Conhost);
        assert_eq!(plan.target, niu);
        assert_eq!(plan.arguments, "");
        assert_eq!(plan.working_directory, home);
    }

    #[test]
    fn profile_registered_gate_matches_wt_form_eligibility() {
        // The full eligibility rule (wt_form_eligible): wt.exe resolved AND
        // the Niubash profile registered in one of the candidate
        // settings.json files. Stage a profileless machine and a registered
        // one; only the latter may take the WT form.
        let temp = tempfile::tempdir().unwrap();
        let settings = temp.path().join("settings.json");
        std::fs::write(
            &settings,
            serde_json::to_string_pretty(&json!({
                "profiles": {
                    "list": [
                        { "guid": "{61c54bbd-c2c6-5271-96e7-009a87ff44bf}",
                          "name": "Windows PowerShell" }
                    ]
                }
            }))
            .unwrap(),
        )
        .unwrap();
        let wt = Path::new("C:/Users/me/AppData/Local/Microsoft/WindowsApps/wt.exe");

        // Profile absent -> not eligible even though wt.exe resolved.
        assert!(!crate::windows_terminal::niubash_profile_registered_in(
            std::slice::from_ref(&settings)
        ));
        assert!(!wt_form_eligible(Some(wt), false));

        // Profile present in the settings file -> eligible.
        std::fs::write(
            &settings,
            serde_json::to_string_pretty(&json!({
                "profiles": {
                    "list": [ { "guid": NIU_PROFILE_GUID, "name": NIU_PROFILE_NAME } ]
                }
            }))
            .unwrap(),
        )
        .unwrap();
        assert!(crate::windows_terminal::niubash_profile_registered_in(&[
            settings
        ]));
        assert!(wt_form_eligible(Some(wt), true));
        // And a WT-less machine is never eligible, registered or not.
        assert!(!wt_form_eligible(None, true));
    }
}
