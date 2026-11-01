//! Regression gate for the in-process readline ABI export (niubash#775,
//! ruline#8).
//!
//! `build.rs` publishes the whole ruline `readline-abi` C surface from the
//! `niu` executable (`-rdynamic` on ELF, `-export_dynamic` on Mach-O, a
//! generated `.def` on MSVC, `--export-all-symbols` on MinGW) so `dlopen`ed
//! bash loadable builtins (`enable -f`) resolve `rl_*` / `history_*` against
//! the live editor. This test inspects the *dynamic* symbol table of the
//! binary Cargo just built for this integration test and fails if any entry
//! in `scripts/readline-abi-exports.txt` disappears.
//!
//! On Windows the check needs `dumpbin` (Visual Studio's C++ toolchain). It
//! is not on `PATH` in a plain shell, so a missing `dumpbin` skips with a
//! clear message instead of failing: `cargo test` must stay runnable on a
//! machine without the MSVC developer environment.

use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

/// The checked-in inventory `build.rs` reads. One `F <name>` / `D <name>`
/// entry per line; comments and blank lines ignored.
const EXPORTS_FILE: &str = "scripts/readline-abi-exports.txt";

/// The symbols `scripts/readline-plugin-probes/loadable_builtin.c` resolves
/// through `dlsym(RTLD_DEFAULT)`, plus a representative slice of the history
/// API. Kept as a focused test so a failure names the plugin-facing set
/// without printing all ~274 missing symbols.
const PLUGIN_PROBE_SYMBOLS: [&str; 10] = [
    "rl_readline_version",
    "rl_library_version",
    "rl_add_defun",
    "rl_bind_key",
    "rl_line_buffer",
    "rl_point",
    "rl_end",
    "add_history",
    "history_length",
    "history_list",
];

/// Parse the checked-in inventory. Mirrors `build.rs`'s parser: a malformed
/// line fails here too, rather than silently auditing a shorter list.
fn read_exports() -> Vec<String> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(EXPORTS_FILE);
    let text = fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));

    let mut exports = Vec::new();
    let mut seen = HashSet::new();
    for (index, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut fields = line.split_whitespace();
        let kind = fields.next();
        assert!(
            matches!(kind, Some("F") | Some("D")),
            "{}:{}: expected `F` or `D`, found {kind:?}",
            path.display(),
            index + 1
        );
        let name = fields.next().unwrap_or_else(|| {
            panic!(
                "{}:{}: missing symbol name after the kind",
                path.display(),
                index + 1
            )
        });
        assert!(
            fields.next().is_none(),
            "{}:{}: expected two fields (`kind name`)",
            path.display(),
            index + 1
        );
        assert!(
            seen.insert(name.to_string()),
            "{}:{}: duplicate entry for {name}",
            path.display(),
            index + 1
        );
        exports.push(name.to_string());
    }

    // The ruline surface is ~274 symbols today. A much shorter file means
    // it was truncated or an nm parse went wrong; that must not pass as
    // "everything exported".
    assert!(
        exports.len() >= 250,
        "{}: only {} entries; expected the full ruline ABI (~274)",
        path.display(),
        exports.len()
    );
    exports
}

/// Names found in a symbol listing, one per whitespace-separated token.
/// `nm`, `readelf` and `dumpbin` all put the (possibly decorated) symbol
/// name somewhere in the line, so token membership is enough — and avoids
/// matching `rl_library_version` as a substring of another symbol. Strip the
/// Mach-O leading underscore and any ELF `@VERSION` suffix.
fn listed_symbols(listing: &str) -> HashSet<String> {
    listing
        .split_whitespace()
        .map(|token| {
            let token = token.trim_start_matches('_');
            token.split_once('@').map_or(token, |(name, _)| name)
        })
        .filter(|token| !token.is_empty())
        .map(str::to_string)
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

/// The built binary and its exported names. `Ok(None)` means the platform
/// tool needed for the inspection is absent (Windows without dumpbin).
fn exported_symbols() -> Result<Option<(String, HashSet<String>)>, String> {
    let binary = env!("CARGO_BIN_EXE_niu");

    let listing = if cfg!(windows) {
        match windows_listing(binary)? {
            Some(listing) => listing,
            None => return Ok(None),
        }
    } else {
        unix_listing(binary)?
    };

    Ok(Some((binary.to_string(), listed_symbols(&listing))))
}

fn missing_from(exported: &HashSet<String>, wanted: &[String]) -> Vec<String> {
    wanted
        .iter()
        .filter(|symbol| !exported.contains(*symbol))
        .cloned()
        .collect()
}

#[test]
fn readline_abi_full_surface_is_dynamically_exported() {
    let Some((binary, exported)) = exported_symbols().unwrap_or_else(|error| panic!("{error}"))
    else {
        return;
    };

    let wanted = read_exports();
    let missing = missing_from(&exported, &wanted);

    assert!(
        missing.is_empty(),
        "{} of {} readline ABI symbols missing from the dynamic symbol table of {binary}:\n{}\n\
         build.rs must anchor every entry in {EXPORTS_FILE} (-rdynamic/-export_dynamic/.def), \
         and the ruline `readline-abi` crate must stay linked (main.rs calls rl_initialize).\n\
         Regenerate the list with scripts/gen-readline-abi-exports.sh if the ruline ABI moved.",
        missing.len(),
        wanted.len(),
        missing.join("\n")
    );
}

#[test]
fn readline_abi_plugin_probe_symbols_are_dynamically_exported() {
    let Some((binary, exported)) = exported_symbols().unwrap_or_else(|error| panic!("{error}"))
    else {
        return;
    };

    let wanted: Vec<String> = PLUGIN_PROBE_SYMBOLS
        .iter()
        .map(|symbol| symbol.to_string())
        .collect();
    let missing = missing_from(&exported, &wanted);

    assert!(
        missing.is_empty(),
        "loadable-builtin probe symbols missing from the dynamic symbol table of {binary}: \
        {missing:?}\n\
         scripts/readline-plugin-probes/loadable_builtin.c resolves these with \
         dlsym(RTLD_DEFAULT); without them the `loadable-builtin-niu` probe reports \
         add_defun=missing bind_key=missing line_buffer=absent."
    );
}
