//! In-process GNU readline C ABI surface (niubash#775, ruline#8).
//!
//! niu embeds the line editor in the executable. Bash loadable builtins
//! (`enable -f ./mybuiltin.so mybuiltin`) are `dlopen`ed into that *same*
//! process and their `rl_*` / `history_*` references resolve against the
//! main executable's dynamic symbol table, so they talk to the live editor
//! instead of loading a second readline instance. Making that work needs two
//! halves:
//!
//! 1. the definitions must survive into the final link (this module), and
//! 2. the link must publish them for the dynamic linker (`build.rs`:
//!    `-rdynamic` on ELF, a `.def` on MSVC, `--export-all-symbols` on
//!    MinGW).
//!
//! This module is deliberately a **visibility probe, not the ABI**. Only the
//! two symbols below are real. The full surface — `readline()`, history,
//! keymaps, funmaps, hooks, completion — belongs to the ruline `readline-abi`
//! crate (tracker: <https://github.com/unixwin/ruline/issues/8>), which the
//! host will re-export and route to the suspended Reedline editor. Keep that
//! boundary: do not grow implementations here.
//!
//! Retention has two layers because `#[no_mangle]` alone is not enough:
//!
//! * `#[used]` puts the definitions in the object and (on ELF) marks them
//!   retained, so LLVM's `--gc-sections` cannot drop them; but a static
//!   library member is only extracted when something references it, and no
//!   Rust code does.
//! * `build.rs` therefore anchors the names explicitly
//!   (`-Wl,--undefined=<sym>` on ELF, `-Wl,-u,_<sym>` on Mach-O,
//!   `/INCLUDE:<sym>` plus the `.def` on MSVC), which pulls the archive
//!   member into the link even under thin LTO.

use std::os::raw::{c_char, c_int};

/// GNU readline's `rl_readline_version`: the ABI revision packed as
/// `0xMMmm`, so `0x0802` is readline 8.2. The probe advertises the revision
/// niu targets; the ruline ABI crate will own the value once the real
/// surface lands.
#[no_mangle]
#[used]
pub static rl_readline_version: c_int = 0x0802;

/// The backing bytes for [`rl_library_version`] (`b"8.2\0"`).
const LIBRARY_VERSION: &[u8] = b"8.2\0";

/// GNU readline's `extern const char *rl_library_version;` — a
/// pointer-valued data symbol, not a string. It has to be `static mut`:
/// raw pointers are not `Sync`, so a shared `static` does not compile. The
/// pointer is initialized at link time and never written afterwards;
/// plugins read it through the C declaration.
#[no_mangle]
#[used]
pub static mut rl_library_version: *const c_char = LIBRARY_VERSION.as_ptr().cast();

#[cfg(test)]
mod tests {
    use super::{rl_library_version, rl_readline_version, LIBRARY_VERSION};
    use std::ffi::CStr;

    #[test]
    fn version_symbols_carry_the_advertised_revision() {
        assert_eq!(rl_readline_version, 0x0802);
        assert_eq!(LIBRARY_VERSION, b"8.2\0");

        // `&raw const` avoids creating a reference to a `static mut`
        // (`static_mut_refs`); only the pointer value is read.
        let pointer = unsafe { (&raw const rl_library_version).read() };
        assert!(!pointer.is_null());
        let version = unsafe { CStr::from_ptr(pointer) };
        assert_eq!(version.to_str().unwrap(), "8.2");
    }
}
