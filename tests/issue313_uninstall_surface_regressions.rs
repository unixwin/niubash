//! niubash#313 — Retired payload names must be swept on upgrade AND
//! on uninstall.
//!
//! Inno Setup's [Files] overwrites but never removes: a file an
//! earlier build shipped and the current payload retired survives
//! every upgrade, and — sitting in {app} after uninstall — keeps the
//! directory alive. The fix sweeps the known retired shapes through
//! [InstallDelete] (before extraction, on upgrade) and
//! [UninstallDelete] (after the uninstall log replay), and removes
//! both install dirs from the user PATH in the post-uninstall step.
//! These static guards pin that surface in installer/niubash.iss so
//! a future edit cannot silently drop a sweep.

use std::path::PathBuf;

fn iss() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("installer")
        .join("niubash.iss");
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// The body of one `[Section]` (up to the next section header).
fn section<'a>(text: &'a str, name: &str) -> &'a str {
    let header = format!("[{name}]");
    let start = text
        .find(&header)
        .unwrap_or_else(|| panic!("section {header} missing from niubash.iss"));
    let body = &text[start + header.len()..];
    match body.find("\n[") {
        Some(end) => &body[..end],
        None => body,
    }
}

const RETIRED_SHAPES: &[&str] = &[
    "{app}\\niubash.exe",
    "{app}\\niu.exe.old-*",
    "{app}\\winuxcmd\\bin\\*.bak-*",
    "{app}\\winuxcmd\\bin\\*.old-*",
    "{app}\\winuxcmd\\bin\\*.niu-old-*",
    "{app}\\winuxcmd\\usr\\bin\\*.bak-*",
    "{app}\\winuxcmd\\usr\\bin\\*.old-*",
    "{app}\\winuxcmd\\usr\\bin\\*.niu-old-*",
];

#[test]
fn install_delete_sweeps_retired_shapes() {
    let text = iss();
    let body = section(&text, "InstallDelete");
    for shape in RETIRED_SHAPES {
        assert!(
            body.contains(&format!("Name: \"{shape}\"")),
            "[InstallDelete] must sweep {shape}"
        );
    }
}

#[test]
fn uninstall_delete_sweeps_retired_shapes() {
    let text = iss();
    let body = section(&text, "UninstallDelete");
    for shape in RETIRED_SHAPES {
        assert!(
            body.contains(&format!("Name: \"{shape}\"")),
            "[UninstallDelete] must sweep {shape}"
        );
    }
}

#[test]
fn post_uninstall_removes_app_dirs_from_user_path() {
    let text = iss();
    assert!(
        text.contains("procedure RemoveAppDirFromUserPath"),
        "the PATH cleanup procedure must exist"
    );
    assert!(
        text.contains("RemoveAppDirFromUserPath();"),
        "the PATH cleanup must actually be called"
    );
    assert!(
        text.contains("usPostUninstall"),
        "the cleanup must be wired to the post-uninstall step"
    );
    assert!(
        text.contains("WinuxCmdBinDir"),
        "the winuxcmd bin dir must be part of the PATH sweep"
    );
}
