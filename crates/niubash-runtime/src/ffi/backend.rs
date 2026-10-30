//! The injectable backend seam behind the provider.
//!
//! A backend owns everything platform- and DLL-specific: loading,
//! the ABI handshake, and the raw call. The provider owns policy
//! (routing, locking, cancellation, fallback mapping) and never
//! touches a library handle itself — which is what lets the unit
//! tests drive the whole provider against a mock backend and
//! assert the ABI context field by field without a real DLL.
//!
//! Result-code discipline (the ABI header's contract): applet
//! exit codes are `>= 0` and pass through unchanged; FFI-layer
//! failures are `<= -1000` ([`ERR_FLOOR`]) and mean "fall back to
//! spawn". The two ranges can never collide.

use std::ffi::c_void;

use super::context::WideContext;

/// The ABI version this host speaks (`WINUX_FFI_ABI_VERSION`).
pub const FFI_ABI_VERSION: u32 = 2;

/// `WINUX_FFI_ERR_CANCELLED`: the cancel callback was already set
/// when the call was entered.
pub const ERR_CANCELLED: i32 = -1006;

/// Applet codes are `>= 0`; any code `<= ERR_FLOOR` is an
/// FFI-layer failure and the host must fall back to spawn.
pub const ERR_FLOOR: i32 = -1000;

/// Cooperative cancellation callback (the ABI's
/// `winux_ffi_cancel_fn`): non-zero return means "stop at the
/// next cooperative checkpoint". `user_data` is the opaque
/// pointer from the context — the provider's trampoline reads an
/// `AtomicBool` through it.
pub type CancelFn = unsafe extern "C" fn(user_data: *mut c_void) -> i32;

/// Arguments for the one-time backend initialization, in owned
/// wide form (the ABI's `winux_ffi_init_args` borrows them for
/// the duration of the init call only).
pub struct InitRequest {
    /// Explicit installation root of the WinuxCmd tree, absolute.
    pub install_root: Vec<u16>,
    /// Locale snapshot: effective `WINUX_LANG`, else `LC_ALL`,
    /// else `LANG`; `None` when all are unset.
    pub locale_stamp: Option<Vec<u16>>,
}

pub trait FfiBackend: Send {
    /// The backend's ABI version (the ABI requires this be
    /// checked before any other call). `Err` = cannot even ask
    /// (not loaded, wrong platform) — an unavailable state, not a
    /// crash.
    fn abi_version(&mut self) -> Result<u32, String>;

    /// One-time initialization with the explicit install root and
    /// locale snapshot. Returns the raw ABI code (0 = success,
    /// negative = FFI-layer error).
    fn init(&mut self, args: &InitRequest) -> Result<i32, String>;

    /// Execute one applet. `ctx` is the owned wide context;
    /// `cancel`/`cancel_user_data` are installed into the ABI
    /// context verbatim. Returns the raw ABI code: the applet's
    /// exit code (>= 0) or an FFI-layer error (<= [`ERR_FLOOR`]).
    fn call(
        &mut self,
        ctx: &WideContext,
        cancel: CancelFn,
        cancel_user_data: *mut c_void,
    ) -> Result<i32, String>;
}

/// The off-Windows backend: there is no `winuxcore.dll` to load,
/// so every operation reports unavailable and the provider
/// declines to spawn — the only correct answer on this platform.
/// Route decisions and the tier machinery stay fully testable
/// here; execution does not exist.
pub struct UnavailableBackend;

impl FfiBackend for UnavailableBackend {
    fn abi_version(&mut self) -> Result<u32, String> {
        Err("winuxcore.dll is a Windows component".to_string())
    }

    fn init(&mut self, _args: &InitRequest) -> Result<i32, String> {
        Err("winuxcore.dll is a Windows component".to_string())
    }

    fn call(
        &mut self,
        _ctx: &WideContext,
        _cancel: CancelFn,
        _cancel_user_data: *mut c_void,
    ) -> Result<i32, String> {
        Err("winuxcore.dll is a Windows component".to_string())
    }
}

#[cfg(windows)]
pub use windows_backend::WindowsDllBackend;

#[cfg(windows)]
mod windows_backend {
    use super::*;
    use crate::ffi::context::RawContext;
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;
    use windows_sys::Win32::Foundation::HMODULE;
    use windows_sys::Win32::System::LibraryLoader::{
        GetProcAddress, LoadLibraryExW, LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR,
        LOAD_LIBRARY_SEARCH_SYSTEM32,
    };

    /// `winux_ffi_init_args` (ABI v2), layout-verbatim.
    #[repr(C)]
    struct WinuxFfiInitArgs {
        struct_size: u32,
        abi_version: u32,
        install_root: *const u16,
        locale_stamp: *const u16,
    }

    /// `winux_ffi_context` (ABI v2), layout-verbatim.
    #[repr(C)]
    struct WinuxFfiContext {
        struct_size: u32,
        abi_version: u32,
        applet: *const u16,
        argc: i32,
        argv: *const *const u16,
        env_block: *const u16,
        cwd: *const u16,
        stdin_handle: *mut c_void,
        stdout_handle: *mut c_void,
        stderr_handle: *mut c_void,
        cancel_check: Option<CancelFn>,
        cancel_user_data: *mut c_void,
    }

    type AbiVersionFn = unsafe extern "C" fn() -> u32;
    type InitFn = unsafe extern "C" fn(*const WinuxFfiInitArgs) -> i32;
    type CallFn = unsafe extern "C" fn(*const WinuxFfiContext) -> i32;

    /// The production backend: `LoadLibraryExW` with the DLL's own
    /// directory and System32 as the only search locations (the
    /// CWD/PATH legs of the default search order stay off — DESIGN
    /// §3.2 item 3), loaded once per process and never unloaded
    /// (updates land in a new version directory and take effect in
    /// the next process; DESIGN §4.2).
    ///
    /// No hash, signature, or integrity check is performed at load
    /// time — deliberately: the trust red line (2026-10-28)
    /// forbids load-time re-verification. The only gate is the
    /// ABI version compatibility check in the provider.
    pub struct WindowsDllBackend {
        /// Kept for the process lifetime: the module is never
        /// unloaded, and this field is the anchor that documents
        /// (and enforces) it.
        #[allow(dead_code)]
        module: HMODULE,
        abi_version_fn: AbiVersionFn,
        init_fn: InitFn,
        call_fn: CallFn,
    }

    impl WindowsDllBackend {
        pub fn load(dll_path: &Path) -> Result<Self, String> {
            let wide: Vec<u16> = dll_path
                .as_os_str()
                .encode_wide()
                .chain(std::iter::once(0))
                .collect();
            // SAFETY: `wide` is a live NUL-terminated wide path
            // for the duration of the call; the flags restrict the
            // search to the DLL's directory + System32.
            let module = unsafe {
                LoadLibraryExW(
                    wide.as_ptr(),
                    std::ptr::null_mut(),
                    LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_SYSTEM32,
                )
            };
            if module.is_null() {
                return Err(format!("LoadLibraryExW failed for {}", dll_path.display()));
            }
            // SAFETY: the module was just loaded by us and is
            // never unloaded, so its exports stay valid for the
            // process lifetime.
            unsafe {
                let abi_version_fn = resolve::<AbiVersionFn>(module, b"winux_ffi_abi_version\0")?;
                let init_fn = resolve::<InitFn>(module, b"winux_ffi_init\0")?;
                let call_fn = resolve::<CallFn>(module, b"winux_ffi_call\0")?;
                Ok(WindowsDllBackend {
                    module,
                    abi_version_fn,
                    init_fn,
                    call_fn,
                })
            }
        }
    }

    /// # Safety
    /// `module` must be a live loaded module containing `name` as
    /// an exported function of type `F`.
    unsafe fn resolve<F>(module: HMODULE, name: &[u8]) -> Result<F, String> {
        let proc = unsafe { GetProcAddress(module, name.as_ptr()) };
        match proc {
            Some(ptr) => {
                // SAFETY: caller guarantees the export's type; the
                // byte-sized transmute is the standard GetProcAddress
                // idiom (F is a fn pointer, same size as the FARPROC).
                Ok(unsafe { std::mem::transmute_copy(&ptr) })
            }
            None => Err(format!(
                "winuxcore.dll is missing the {} export",
                String::from_utf8_lossy(&name[..name.len() - 1])
            )),
        }
    }

    impl FfiBackend for WindowsDllBackend {
        fn abi_version(&mut self) -> Result<u32, String> {
            // SAFETY: the export was resolved at load; the DLL is
            // never unloaded.
            Ok(unsafe { (self.abi_version_fn)() })
        }

        fn init(&mut self, args: &InitRequest) -> Result<i32, String> {
            let root_ptr = if args.install_root.is_empty() {
                std::ptr::null()
            } else {
                args.install_root.as_ptr()
            };
            let locale_ptr = args
                .locale_stamp
                .as_ref()
                .map(|v| v.as_ptr())
                .unwrap_or(std::ptr::null());
            let init_args = WinuxFfiInitArgs {
                struct_size: std::mem::size_of::<WinuxFfiInitArgs>() as u32,
                abi_version: FFI_ABI_VERSION,
                install_root: root_ptr,
                locale_stamp: locale_ptr,
            };
            // SAFETY: every pointer borrows from `args` / the
            // stack struct, both live for the call; the ABI keeps
            // nothing after it returns.
            Ok(unsafe { (self.init_fn)(&init_args) })
        }

        fn call(
            &mut self,
            ctx: &WideContext,
            cancel: CancelFn,
            cancel_user_data: *mut c_void,
        ) -> Result<i32, String> {
            let raw: RawContext = ctx.as_raw();
            let context = WinuxFfiContext {
                struct_size: std::mem::size_of::<WinuxFfiContext>() as u32,
                abi_version: FFI_ABI_VERSION,
                applet: raw.applet,
                argc: raw.argc,
                argv: raw.argv,
                env_block: raw.env_block,
                cwd: raw.cwd,
                stdin_handle: raw.stdin as *mut c_void,
                stdout_handle: raw.stdout as *mut c_void,
                stderr_handle: raw.stderr as *mut c_void,
                cancel_check: Some(cancel),
                cancel_user_data,
            };
            // SAFETY: `context` and everything it points at (the
            // owned `ctx`) live for the call; the host flush rule
            // and the FFI lock are the provider's side of the
            // contract, already held by the caller.
            Ok(unsafe { (self.call_fn)(&context) })
        }
    }
}
