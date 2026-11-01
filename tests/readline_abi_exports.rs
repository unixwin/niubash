//! Regression gate for the in-process readline ABI export (niubash#775,
//! ruline#8).
//!
//! `build.rs` publishes the `readline_abi` probe symbols from the `niu`
//! executable (`-rdynamic` on ELF, `-export_dynamic` on Mach-O, a `.def` on
//! MSVC, `--export-all-symbols` on MinGW) so `dlopen`ed bash loadable
//! builtins (`enable -f`) resolve `rl_*` against the live editor. This test
//! inspects the *dynamic* symbol table of the binary Cargo just built for
//! this integration test and fails if a symbol disappears.
//!
//! On Windows the check needs `dumpbin` (Visual Studio's C++ toolchain). It
//! is not on `PATH` in a plain shell, so a missing `dumpbin` skips with a
//! clear message instead of failing: `cargo test` must stay runnable on a
//! machine without the MSVC developer environment.

use std::collections::HashSet;
use std::process::Command;

/// Must match `readline_abi.rs` and `READLINE_ABI_PROBE_SYMBOLS` in
/// `build.rs`. The full ruline ABI replaces this list; it does not grow here.
const PROBE_SYMBOLS: [&str; 2] = ["rl_readline_version", "rl_library_version"];

/// Names found in a symbol listing, one per whitespace-separated token.
/// `nm`, `readelf` and `dumpbin` all put the (possibly decorated) symbol
/// name somewhere in the line, so token membership is enough — and avoids
/// matching `rl_library_version` as a substring of another symbol.
fn listed_symbols(listing: &str) -> HashSet<&str> {
    listing
        .split_whitespace()
        .map(|token| token.trim_start_matches('_'))
        .collect()
}

fn run(program: &str, args: &[&str]) -> Option<String> {
    let output = Command::new(program).args(args).output().ok()?;
    if !output.status.success() {
        eprintln!(
            "{program} {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        );
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Dynamic-table listing on Unix. ELF: `nm -D` (dynamic symbols only) with a
/// `readelf --dyn-syms` fallback for hosts whose nm predates `-D`. Mach-O:
/// `nm -gU` (global, defined — the executable's exported set).
fn unix_listing(binary: &str) -> Result<String, String> {
    if cfg!(target_os = "macos") {
        return run("nm", &["-gU", binary]).ok_or_else(|| {
            "nm -gU failed (is the Xcode command-line toolchain installed?)".into()
        });
    }

    if let Some(listing) = run("nm", &["-D", "--defined-only", binary]) {
        return Ok(listing);
    }
    run("readelf", &["--dyn-syms", "--wide", binary])
        .ok_or_else(|| "neither `nm -D` nor `readelf --dyn-syms` could read the binary".to_string())
}

fn windows_listing(binary: &str) -> Result<Option<String>, String> {
    match run("dumpbin", &["/nologo", "/exports", binary]) {
        Some(listing) => Ok(Some(listing)),
        None => {
            // Distinguish "dumpbin not installed" (skip) from "dumpbin ran
            // and found nothing" (fail). `run` already printed stderr for
            // the latter; probe availability once more to classify.
            if Command::new("dumpbin").arg("/?").output().is_err() {
                eprintln!(
                    "skipping readline ABI export check: `dumpbin` not found on PATH; \
                     run this test from a Visual Studio developer prompt to enforce it"
                );
                Ok(None)
            } else {
                Err("dumpbin /exports did not produce a listing".to_string())
            }
        }
    }
}

#[test]
fn readline_abi_probe_symbols_are_dynamically_exported() {
    let binary = env!("CARGO_BIN_EXE_niu");

    let listing = if cfg!(windows) {
        match windows_listing(binary) {
            Ok(Some(listing)) => listing,
            Ok(None) => return,
            Err(error) => panic!("{error}"),
        }
    } else {
        unix_listing(binary).unwrap_or_else(|error| panic!("{error}"))
    };

    let exported = listed_symbols(&listing);
    let missing: Vec<_> = PROBE_SYMBOLS
        .iter()
        .filter(|symbol| !exported.contains(**symbol))
        .copied()
        .collect();

    assert!(
        missing.is_empty(),
        "readline ABI symbols missing from the dynamic symbol table of {binary}: {missing:?}\n\
         build.rs must pass -rdynamic/-export_dynamic/.def for the niu bin, and the \
         definitions in crates/niubash-runtime/src/readline_abi.rs must stay #[no_mangle] + #[used].\n\
         Listing:\n{listing}"
    );
}
