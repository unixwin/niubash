//! The ABI v2 call context (WinuxCmd `winux_ffi.h`), built
//! field-by-field from the executor-resolved call:
//!
//! - `applet`: the canonical tier-table spelling of the command
//!   name (a routing token — the ABI's argv excludes the name, so
//!   nothing the user typed is rewritten by using it).
//! - `argv`: the argument vector exactly as resolved — wide
//!   strings, no insertion, removal, reordering, or annotation
//!   (DESIGN §1.1 red line: a command that would need host-side
//!   argument massaging to stay equivalent is FFI-ineligible,
//!   permanently; the host never massages).
//! - `env_block`: the complete environment for the call —
//!   including prefix assignments the executor resolved — as one
//!   wide double-NUL-terminated block of `KEY=VALUE` entries, in
//!   the order the executor composed it. An empty environment is
//!   the header's empty block (a single NUL pair).
//! - `cwd`: the call's working directory, absolute and non-empty;
//!   the ABI makes NULL a contract violation, so a relative or
//!   empty cwd is a build error here, never a silent NULL.
//! - stdio: the host's already-resolved handles, borrowed raw
//!   (0 = NULL = "keep the current process handle", the ABI's own
//!   convention). No capture layer, no copy-back.
//!
//! [`WideContext`] owns every buffer; [`RawContext`] is the
//! borrowed pointer view handed to the backend for the duration
//! of one call.

use std::path::{Path, PathBuf};

/// Raw stdio handles for the call, as OS handle values
/// (`HANDLE` on Windows). `0` means NULL in the ABI.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StdHandles {
    pub stdin: usize,
    pub stdout: usize,
    pub stderr: usize,
}

/// One executor-resolved call, in host terms.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FfiRequest {
    /// Bare command name as the user invoked it (route input).
    pub name: String,
    /// Arguments, already expanded by the executor, in order.
    pub args: Vec<String>,
    /// The complete environment for the call, in composition
    /// order, prefix assignments included.
    pub env: Vec<(String, String)>,
    /// Working directory for the call; must be absolute.
    pub cwd: PathBuf,
    pub std: StdHandles,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContextError {
    /// The applet name must not be empty.
    EmptyApplet,
    /// The ABI requires a non-NULL absolute cwd.
    CwdNotAbsolute(PathBuf),
}

impl ContextError {
    pub fn describe(&self) -> String {
        match self {
            ContextError::EmptyApplet => "empty applet name".to_string(),
            ContextError::CwdNotAbsolute(path) => {
                format!("cwd {} is not absolute", path.display())
            }
        }
    }
}

fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// The environment block: each `KEY=VALUE` entry NUL-terminated,
/// plus the final terminating NUL. An empty environment yields
/// exactly `[0, 0]` — the header's "single NUL pair" (the memory
/// image of the wide literal `L"\0"`).
pub fn build_env_block(env: &[(String, String)]) -> Vec<u16> {
    let mut block = Vec::new();
    for (key, value) in env {
        block.extend(format!("{key}={value}").encode_utf16());
        block.push(0);
    }
    block.push(0);
    if env.is_empty() {
        block.push(0);
    }
    block
}

/// Owned wide storage + the borrowed pointer view over it.
pub struct WideContext {
    applet: Vec<u16>,
    /// The owned argv buffers. Never read back through the field
    /// — it exists so the buffers the `argv_ptrs` point into stay
    /// alive (and unmoved) for the whole call.
    #[allow(dead_code)]
    argv_storage: Vec<Vec<u16>>,
    argv_ptrs: Vec<*const u16>,
    env_block: Vec<u16>,
    cwd: Vec<u16>,
    std: StdHandles,
}

/// The pointer view handed to a backend for one call. Every
/// pointer borrows from the [`WideContext`] that produced it;
/// the context must outlive the call (the ABI's ownership rule).
pub struct RawContext {
    pub applet: *const u16,
    pub argc: i32,
    pub argv: *const *const u16,
    pub env_block: *const u16,
    pub cwd: *const u16,
    pub stdin: usize,
    pub stdout: usize,
    pub stderr: usize,
}

impl WideContext {
    pub fn build(applet: &str, request: &FfiRequest) -> Result<Self, ContextError> {
        if applet.is_empty() {
            return Err(ContextError::EmptyApplet);
        }
        if !request.cwd.is_absolute() || request.cwd.as_os_str().is_empty() {
            return Err(ContextError::CwdNotAbsolute(request.cwd.clone()));
        }
        // Build all owned storage first; the pointer table is
        // computed afterwards so no buffer can move under a taken
        // pointer (each inner Vec keeps its own heap buffer even
        // when the outer Vec grows).
        let argv_storage: Vec<Vec<u16>> = request.args.iter().map(|a| to_wide(a)).collect();
        let argv_ptrs: Vec<*const u16> = argv_storage.iter().map(|a| a.as_ptr()).collect();
        Ok(WideContext {
            applet: to_wide(applet),
            argv_storage,
            argv_ptrs,
            env_block: build_env_block(&request.env),
            cwd: to_wide(&path_to_wide_string(&request.cwd)),
            std: request.std,
        })
    }

    pub fn as_raw(&self) -> RawContext {
        RawContext {
            applet: self.applet.as_ptr(),
            argc: self.argv_ptrs.len() as i32,
            argv: if self.argv_ptrs.is_empty() {
                std::ptr::null()
            } else {
                self.argv_ptrs.as_ptr()
            },
            env_block: self.env_block.as_ptr(),
            cwd: self.cwd.as_ptr(),
            stdin: self.std.stdin,
            stdout: self.std.stdout,
            stderr: self.std.stderr,
        }
    }

    /// The owned env block (tests assert it byte-for-byte).
    pub fn env_block(&self) -> &[u16] {
        &self.env_block
    }
}

/// The cwd as the wide string the ABI receives. On Windows the
/// executor's cwd is already a native absolute path; rendering it
/// losslessly is the seam's job there. Here the lossy rendering is
/// used only for the (non-Windows) test/measurement path — the
/// production backend is Windows-only.
fn path_to_wide_string(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request() -> FfiRequest {
        FfiRequest {
            name: "grep".to_string(),
            args: vec!["-n".to_string(), "a b".to_string(), String::new()],
            env: vec![
                ("PATH".to_string(), "C:\\bin".to_string()),
                ("FOO".to_string(), "bar baz".to_string()),
            ],
            cwd: PathBuf::from(if cfg!(windows) { "C:\\work" } else { "/work" }),
            std: StdHandles {
                stdin: 11,
                stdout: 22,
                stderr: 33,
            },
        }
    }

    /// Read a NUL-terminated wide string back (the mock-backend
    /// view of the ABI memory).
    ///
    /// # Safety
    /// `ptr` must point at a live NUL-terminated u16 buffer.
    pub(crate) unsafe fn read_wide(ptr: *const u16) -> String {
        let mut units = Vec::new();
        let mut cursor = ptr;
        loop {
            let unit = unsafe { *cursor };
            if unit == 0 {
                break;
            }
            units.push(unit);
            cursor = unsafe { cursor.add(1) };
        }
        String::from_utf16_lossy(&units)
    }

    #[test]
    fn argv_is_byte_for_byte_and_in_order() {
        let req = request();
        let ctx = WideContext::build("grep", &req).expect("build");
        let raw = ctx.as_raw();
        assert_eq!(raw.argc, 3);
        unsafe {
            let argv = std::slice::from_raw_parts(raw.argv, raw.argc as usize);
            assert_eq!(read_wide(argv[0]), "-n");
            assert_eq!(read_wide(argv[1]), "a b");
            assert_eq!(read_wide(argv[2]), "");
            assert_eq!(read_wide(raw.applet), "grep");
            assert_eq!(
                read_wide(raw.cwd),
                if cfg!(windows) { "C:\\work" } else { "/work" }
            );
        }
        assert_eq!(raw.stdin, 11);
        assert_eq!(raw.stdout, 22);
        assert_eq!(raw.stderr, 33);
    }

    #[test]
    fn empty_args_gives_null_argv_and_zero_argc() {
        let mut req = request();
        req.args.clear();
        let ctx = WideContext::build("grep", &req).expect("build");
        let raw = ctx.as_raw();
        assert_eq!(raw.argc, 0);
        assert!(raw.argv.is_null());
    }

    #[test]
    fn env_block_is_complete_double_nul_terminated_in_order() {
        let req = request();
        let block = build_env_block(&req.env);
        let expected: Vec<u16> = "PATH=C:\\bin"
            .encode_utf16()
            .chain([0])
            .chain("FOO=bar baz".encode_utf16())
            .chain([0, 0])
            .collect();
        assert_eq!(block, expected);
    }

    #[test]
    fn empty_env_block_is_the_single_nul_pair() {
        assert_eq!(build_env_block(&[]), vec![0, 0]);
    }

    #[test]
    fn relative_or_empty_cwd_is_a_build_error_never_a_null() {
        let mut req = request();
        req.cwd = PathBuf::from("relative/dir");
        assert!(matches!(
            WideContext::build("grep", &req),
            Err(ContextError::CwdNotAbsolute(_))
        ));
        req.cwd = PathBuf::new();
        assert!(WideContext::build("grep", &req).is_err());
    }

    #[test]
    fn empty_applet_is_a_build_error() {
        let req = request();
        assert_eq!(
            WideContext::build("", &req).err(),
            Some(ContextError::EmptyApplet)
        );
    }
}
