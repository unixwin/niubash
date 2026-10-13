//! niubash#211: round-trip proof for the generated shortcuts.
//!
//! The shortcut behavior itself cannot be pinned by a real installer run in
//! CI, so these tests exercise the exact COM path double-clicking uses:
//! write a `.lnk` through [`niubash_runtime::shortcut::write_shortcut`],
//! then load it back through `read_shortcut` and assert the parameters —
//! the WT-form target/arguments, the Niubash icon, the working directory,
//! and the update-only install semantics. A vtable drift in the
//! hand-declared shell-link interfaces fails loudly here.
//!
//! Target and icon files are staged as real files on purpose: the shell
//! link object validates `SetPath` inputs and answers S_FALSE for paths it
//! cannot make sense of. Production never hits that (both `wt.exe` tiers
//! and the conhost target come from `is_file()`-checked candidates), so the
//! tests mirror that contract.

#![cfg(windows)]

use niubash_runtime::shortcut::{
    install_shortcuts, read_shortcut, write_shortcut, ShortcutForm, ShortcutPlan,
};
use std::path::{Path, PathBuf};

/// The shell link object resolves `SetPath` inputs to their canonical
/// spelling, so a target staged under an 8.3 short-name directory
/// (CI checkouts: `RUNNER~1`) reads back long-form (`runneradmin`).
/// Compare canonicalized on both sides — spelling is not the contract,
/// identity is.
fn canonical(path: &Path) -> PathBuf {
    let c = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let s = c.as_os_str().to_string_lossy();
    match s.strip_prefix(r"\\?\") {
        Some(stripped) => PathBuf::from(stripped.to_string()),
        None => c,
    }
}

fn assert_same_path(left: &Path, right: &Path) {
    assert_eq!(canonical(left), canonical(right));
}

/// Stage `wt.exe` / `niu.exe` / icon files so every path a plan carries
/// exists, and return their absolute paths.
fn stage_targets(temp: &Path) -> (PathBuf, PathBuf, PathBuf) {
    let wt = temp.join("WindowsApps").join("wt.exe");
    let niu = temp.join("Niubash").join("niu.exe");
    let icon = temp.join("Niubash").join("assets").join("niubash-icon.ico");
    for file in [&wt, &niu, &icon] {
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, b"staged").unwrap();
    }
    (wt, niu, icon)
}

fn wt_form_plan(temp: &Path, wt: &Path, icon: &Path) -> ShortcutPlan {
    ShortcutPlan {
        form: ShortcutForm::WindowsTerminal {
            wt_exe: wt.to_path_buf(),
        },
        target: wt.to_path_buf(),
        arguments: "-p Niubash".to_string(),
        working_directory: temp.to_path_buf(),
        icon: Some(icon.to_path_buf()),
    }
}

fn conhost_form_plan(temp: &Path, niu: &Path) -> ShortcutPlan {
    ShortcutPlan {
        form: ShortcutForm::Conhost,
        target: niu.to_path_buf(),
        arguments: String::new(),
        working_directory: temp.to_path_buf(),
        icon: Some(niu.to_path_buf()),
    }
}

#[test]
fn wt_form_shortcut_round_trips_target_and_arguments() {
    let temp = tempfile::tempdir().unwrap();
    let (wt, _niu, icon) = stage_targets(temp.path());
    let lnk = temp.path().join("desktop").join("Niubash.lnk");

    write_shortcut(&wt_form_plan(temp.path(), &wt, &icon), &lnk).unwrap();
    let readback = read_shortcut(&lnk).unwrap();

    assert_same_path(&readback.target, &wt);
    assert_eq!(readback.arguments, "-p Niubash");
    assert_same_path(readback.icon.as_deref().unwrap_or(Path::new("")), &icon);
    assert_same_path(&readback.working_directory, temp.path());
}

#[test]
fn conhost_form_shortcut_round_trips_direct_exe() {
    let temp = tempfile::tempdir().unwrap();
    let (_wt, niu, _icon) = stage_targets(temp.path());
    let lnk = temp.path().join("group").join("Niubash.lnk");

    write_shortcut(&conhost_form_plan(temp.path(), &niu), &lnk).unwrap();
    let readback = read_shortcut(&lnk).unwrap();

    assert_same_path(&readback.target, &niu);
    assert_eq!(readback.arguments, "");
    assert_same_path(&readback.working_directory, temp.path());
}

#[test]
fn install_shortcuts_updates_existing_and_skips_absent_links() {
    let temp = tempfile::tempdir().unwrap();
    let (wt, niu, icon) = stage_targets(temp.path());
    let desktop = temp.path().join("Desktop").join("Niubash.lnk");
    let start_menu = temp
        .path()
        .join("Programs")
        .join("Niubash")
        .join("Niubash.lnk");

    // Installer baseline: Inno created the desktop link (conhost form); the
    // Start-menu group link does not exist yet.
    std::fs::create_dir_all(desktop.parent().unwrap()).unwrap();
    write_shortcut(&conhost_form_plan(temp.path(), &niu), &desktop).unwrap();

    // update-only: the existing link is rewritten in the WT form; the
    // absent Start-menu link is skipped, NOT created.
    let summary = install_shortcuts(
        &wt_form_plan(temp.path(), &wt, &icon),
        &desktop,
        &start_menu,
        true,
    )
    .unwrap();
    assert_eq!(summary.written, vec![desktop.clone()]);
    assert_eq!(summary.skipped, vec![start_menu.clone()]);
    assert!(!start_menu.is_file());
    assert_eq!(read_shortcut(&desktop).unwrap().arguments, "-p Niubash");

    // Without update-only both links are created (standalone run).
    let summary = install_shortcuts(
        &wt_form_plan(temp.path(), &wt, &icon),
        &desktop,
        &start_menu,
        false,
    )
    .unwrap();
    assert_eq!(summary.written, vec![desktop.clone(), start_menu.clone()]);
    assert_eq!(read_shortcut(&start_menu).unwrap().arguments, "-p Niubash");
}
