//! Deep-audit A4 regression pins: CLI diagnostic quality on the
//! non-interactive verb surfaces.
//!
//! - A4-04: a corrupt `plugins.json` failed with a bare serde error
//!   that named no file; `load_spec` now carries the path as context.
//! - A4-05: `plugin enable/disable -h|--help` treated the flag as an
//!   operand ("no source or asset named '-h'"); it prints usage now,
//!   like `plugin add -h` (niubash#176).
//! - A4-06: a binary script operand was refused with the bare string
//!   "cannot execute binary file"; the diagnostic names the operand
//!   like its sibling arms (and GNU's shell_execve surface).
//! - A4-10: the top-level error renderer printed only the outermost
//!   anyhow context (`{}`), discarding every attached cause — a
//!   broken collection manifest surfaced as the content-free line
//!   "parsing the collection manifest". The renderer now prints the
//!   chain (`{:#}`).

use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

fn niu_binary() -> PathBuf {
    let p = PathBuf::from(env!("CARGO_BIN_EXE_niu"));
    if p.exists() {
        return p;
    }
    let mut fallback = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    fallback.push("target");
    fallback.push("debug");
    fallback.push(if cfg!(windows) { "niu.exe" } else { "niu" });
    fallback
}

fn temp_home(label: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let home = std::env::temp_dir().join(format!(
        "niu-a4-diag-{label}-{}-{nanos}",
        std::process::id()
    ));
    std::fs::create_dir_all(&home).unwrap();
    home
}

fn run_niu(home: &PathBuf, args: &[&str]) -> Output {
    Command::new(niu_binary())
        .args(args)
        .env_clear()
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

#[test]
fn a4_04_corrupt_spec_error_names_the_file_and_the_cause() {
    let home = temp_home("spec");
    let config_dir = home.join(".niubash");
    std::fs::create_dir_all(&config_dir).unwrap();
    let spec = config_dir.join("plugins.json");
    std::fs::write(&spec, "{not json").unwrap();
    let output = run_niu(&home, &["plugin", "sync"]);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    // The host canonicalizes its home dir before composing the spec path
    // (path_utils::normalize_existing_host_path), so on Windows the error
    // spells the file with the canonicalized prefix (junctions resolved,
    // no \\?\ verbatim marker) rather than the raw temp-dir spelling.
    // Compare canonical forms on both sides; the requirement is that the
    // error names THIS file, not one exact path spelling.
    let canonical_spec = std::fs::canonicalize(&spec)
        .map(|path| {
            let text = path.display().to_string();
            text.strip_prefix(r"\\?\").map(str::to_string).unwrap_or(text)
        })
        .unwrap_or_else(|_| spec.display().to_string());
    let canonical_spec = canonical_spec.replace('\\', "/");
    assert!(
        stderr.replace('\\', "/").contains(&canonical_spec),
        "error must name the spec file: {stderr}"
    );
    assert!(
        stderr.contains("key must be a string"),
        "error must keep the serde cause (A4-10 chain): {stderr}"
    );
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn a4_05_enable_help_is_a_help_request_not_an_operand() {
    let home = temp_home("enable-help");
    for verb in ["enable", "disable"] {
        for flag in ["--help", "-h"] {
            let output = run_niu(&home, &["plugin", verb, flag]);
            assert!(output.status.success(), "{verb} {flag}: {output:?}");
            let stdout = String::from_utf8_lossy(&output.stdout);
            assert!(
                stdout.contains("Usage:"),
                "{verb} {flag} must print usage: {stdout}"
            );
        }
    }
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn a4_06_binary_script_operand_diagnostic_names_the_file() {
    let home = temp_home("binary");
    // An ELF image: /bin/true on Linux, any host binary elsewhere.
    let source = if cfg!(windows) {
        niu_binary()
    } else {
        PathBuf::from("/bin/true")
    };
    let operand = home.join("elf.sh");
    std::fs::copy(&source, &operand).unwrap();
    let output = run_niu(&home, &[operand.to_string_lossy().as_ref()]);
    assert_eq!(output.status.code(), Some(126), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("elf.sh") && stderr.contains("cannot execute binary file"),
        "diagnostic must name the operand: {stderr}"
    );
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn a4_10_broken_collection_manifest_shows_the_serde_cause() {
    let home = temp_home("distro");
    let collection = home.join("collection");
    std::fs::create_dir_all(&collection).unwrap();
    std::fs::write(collection.join("niu-collection.json"), "{broken").unwrap();
    let output = run_niu(
        &home,
        &[
            "plugin",
            "distro",
            "import",
            collection.to_string_lossy().as_ref(),
        ],
    );
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("parsing the collection manifest"),
        "context line present: {stderr}"
    );
    assert!(
        stderr.contains("key must be a string"),
        "the underlying cause must survive to the CLI surface: {stderr}"
    );
    let _ = std::fs::remove_dir_all(&home);
}
