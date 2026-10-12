use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    emit_rubash_revision();

    // Gate on the TARGET OS, not the host: the .res only links into PE
    // binaries. `#[cfg(windows)]` here would describe the host and make
    // `cargo check --target aarch64-linux-android` on a Windows dev box try
    // to run the SDK's arm64 rc.exe, which is an arm64 binary and cannot
    // launch on x64 (os error 216). Hosts cross-checking a windows target
    // from non-windows OSes are not a supported flow (release builds Windows
    // on Windows runners), and they now fail loudly instead of silently
    // skipping the icon.
    if std::env::var_os("CARGO_CFG_TARGET_OS").as_deref() == Some(std::ffi::OsStr::new("windows")) {
        embed_windows_icon();
    }
}

/// Locate a sibling `rubash` checkout. Only consulted when rubash is a path
/// dependency; while it is a `git = "..."` dependency nothing here is compiled
/// and `Cargo.lock` decides the revision.
fn rubash_checkout_dir() -> Option<PathBuf> {
    let manifest_dir = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR")?);
    let sibling = manifest_dir.parent()?.join("rubash");
    sibling.is_dir().then_some(sibling)
}

/// Embed the rubash revision that is actually compiled into this binary.
///
/// `Cargo.lock` is the only source that names the commit Cargo resolves for a
/// `git = "..."` dependency, so it decides. It used to be the fallback, which
/// let a sibling `../rubash` checkout win even though nothing in the build
/// reads that tree: a developer whose local rubash sat on an unrelated commit
/// saw its HEAD printed in the version banner, and the reported revision did
/// not reach the binary at all.
///
/// The checkout stays as the fallback for the path-dependency case, where the
/// lock carries no `source =` line and the local tree really is the build.
fn emit_rubash_revision() {
    println!("cargo:rerun-if-changed=Cargo.lock");

    let revision = rubash_revision_from_lock()
        .or_else(rubash_revision_from_checkout)
        .unwrap_or_else(|| "unknown".to_string());

    println!("cargo:rustc-env=NIU_RUBASH_REV={revision}");
}

/// Ask the sibling checkout for the commit being compiled, marking it dirty
/// when the tree carries uncommitted changes. The dirty marker matters more
/// than the hash here: a tree with uncommitted edits is not reproducible, and
/// the version banner is the only place a user can see that. This describes
/// the build only while rubash is a path dependency.
fn rubash_revision_from_checkout() -> Option<String> {
    let Some(dir) = rubash_checkout_dir() else {
        return None;
    };
    watch_rubash_refs(&dir);

    let head = git(&dir, &["rev-parse", "--short=12", "HEAD"])?;
    if !head.status.success() {
        return None;
    }
    let mut revision = String::from_utf8_lossy(&head.stdout).trim().to_string();
    if revision.is_empty() {
        return None;
    }

    // `git describe --dirty` semantics (builtin/describe.c describe_default,
    // --dirty runs `diff-index --quiet HEAD`): only tracked content that
    // differs from HEAD makes a tree dirty; untracked files never do.
    // `git status --porcelain` alone also counts `??` entries, so a tree
    // whose only changes are untracked scratch files used to bake a bogus
    // `-dirty` into the version banner.
    if let Some(status) = git(&dir, &["status", "--porcelain", "--untracked-files=no"]) {
        if status.status.success() && !String::from_utf8_lossy(&status.stdout).trim().is_empty() {
            revision.push_str("-dirty");
        }
    }
    Some(revision)
}

/// `git` may be absent on a build host, and a source tarball has no `.git`
/// at all. Both are fine: the revision just stays "unknown".
fn git(dir: &Path, args: &[&str]) -> Option<std::process::Output> {
    let mut cmd = Command::new("git");
    cmd.arg("-C");
    cmd.arg(dir);
    for arg in args {
        cmd.arg(arg);
    }
    cmd.output().ok()
}

/// A path dependency does not make Cargo re-run this script when only the
/// sibling checkout's state moves, so watch the git metadata files by hand.
///
/// The revision label has two halves and each needs its own cache key:
///
/// - The commit hash moves with `HEAD`. `HEAD` alone is not enough — on a
///   branch it stays `ref: refs/heads/<branch>` and only the pointed-at ref
///   file changes — so the symref target is watched too, on ANY branch (a
///   lane worktree moves `refs/heads/wt/...`, not master/main). Refs may
///   also live in `packed-refs` instead of a loose file, so that file is
///   watched as well. Per-worktree files (`HEAD`, `index`) live in the
///   worktree's own git dir, refs in the common dir; a linked worktree
///   splits the two.
/// - The `-dirty` half flips without any ref moving (edit, stage, restore,
///   stash, clean). Those operations all rewrite the index — and so does
///   every `git status` stat-cache refresh — so the index is the watch key
///   that keeps a dirty-at-build-time tree from serving a stale `-dirty`
///   after it was cleaned (or vice versa) while HEAD stood still.
fn watch_rubash_refs(dir: &Path) {
    let (git_dir, common_dir) = split_git_dirs(dir);
    for path in [
        git_dir.join("HEAD"),
        git_dir.join("index"),
        common_dir.join("FETCH_HEAD"),
        common_dir.join("packed-refs"),
        common_dir.join("refs/heads/master"),
        common_dir.join("refs/heads/main"),
    ] {
        if path.is_file() {
            println!("cargo:rerun-if-changed={}", path.display());
        }
    }
    // Watch the branch HEAD actually points at, on any branch.
    if let Ok(head) = fs::read_to_string(git_dir.join("HEAD")) {
        if let Some(target) = head.trim().strip_prefix("ref:") {
            let target = target.trim();
            let path = common_dir.join(target);
            if path.is_file() {
                println!("cargo:rerun-if-changed={}", path.display());
            }
        }
    }
}

/// Resolve (per-worktree git dir, common git dir) for a checkout. A plain
/// clone keeps everything in `<dir>/.git`; a linked worktree stores `.git`
/// as a `gitdir:` file pointing at `<main>/.git/worktrees/<name>`, whose
/// `commondir` file names the shared metadata directory.
fn split_git_dirs(dir: &Path) -> (PathBuf, PathBuf) {
    let marker = dir.join(".git");
    if !marker.is_file() {
        let plain = dir.join(".git");
        return (plain.clone(), plain);
    }
    let Some(target) = fs::read_to_string(&marker)
        .ok()
        .map(|text| text.trim().to_string())
        .and_then(|text| {
            text.strip_prefix("gitdir:")
                .map(str::trim)
                .filter(|rest| !rest.is_empty())
                .map(str::to_string)
        })
    else {
        let plain = dir.join(".git");
        return (plain.clone(), plain);
    };
    let git_dir = PathBuf::from(&target);
    let git_dir = if git_dir.is_absolute() {
        git_dir
    } else {
        dir.join(git_dir)
    };
    let common_dir = match fs::read_to_string(git_dir.join("commondir"))
        .ok()
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty())
    {
        Some(text) => {
            let path = PathBuf::from(text);
            if path.is_absolute() {
                path
            } else {
                git_dir.join(path)
            }
        }
        None => git_dir.clone(),
    };
    (git_dir, common_dir)
}

/// `git = "..."` dependencies record their revision in `Cargo.lock`; a path
/// dependency does not. This is the authoritative lookup while rubash is a git
/// dependency, because the lock names the commit Cargo actually compiles.
fn rubash_revision_from_lock() -> Option<String> {
    let lock = fs::read_to_string("Cargo.lock").ok()?;
    let mut in_rubash = false;

    for line in lock.lines() {
        let trimmed = line.trim();
        if trimmed == "[[package]]" {
            in_rubash = false;
            continue;
        }

        if trimmed == "name = \"rubash\"" {
            in_rubash = true;
            continue;
        }

        if in_rubash && trimmed.starts_with("source = ") {
            let source = trimmed.trim_start_matches("source = ").trim_matches('"');
            let rev = source.rsplit_once('#').map_or("unknown", |(_, rev)| rev);
            // Match the 12-character form the checkout path reports.
            return Some(rev.chars().take(12).collect());
        }
    }

    None
}

// Compiled on every host: the only call site is runtime-gated on
// CARGO_CFG_TARGET_OS == "windows" (see main), and gating the definition on
// the host triple instead would break non-Windows hosts that cross-check a
// Windows target (E0425: the call site compiles, the definition does not).
fn embed_windows_icon() {
    use std::env;
    use std::path::{Path, PathBuf};
    use std::process::Command;

    let manifest_dir =
        PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let assets_dir = manifest_dir.join("assets");
    let rc_file = assets_dir.join("niubash.rc");
    let icon_file = assets_dir.join("niubash-icon.ico");
    let out_file = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR")).join("niubash.res");

    println!("cargo:rerun-if-changed={}", rc_file.display());
    println!("cargo:rerun-if-changed={}", icon_file.display());

    let rc_exe = find_resource_compiler().unwrap_or_else(|| {
        panic!(
            "could not find rc.exe or llvm-rc.exe; install the Windows SDK or LLVM resource compiler to embed niu.exe icon"
        )
    });

    let status = Command::new(&rc_exe)
        .current_dir(&assets_dir)
        .arg("/nologo")
        .arg(format!("/fo{}", out_file.display()))
        .arg(rc_file.file_name().expect("resource file name"))
        .status()
        .unwrap_or_else(|err| panic!("failed to run {}: {err}", rc_exe.display()));

    if !status.success() {
        panic!("{} failed with status {status}", rc_exe.display());
    }

    println!("cargo:rustc-link-arg-bin=niu={}", out_file.display());

    fn find_resource_compiler() -> Option<PathBuf> {
        find_in_path("rc.exe")
            .or_else(|| find_in_path("llvm-rc.exe"))
            .or_else(find_windows_sdk_rc)
    }

    fn find_in_path(exe: &str) -> Option<PathBuf> {
        let path = std::env::var_os("PATH")?;
        std::env::split_paths(&path)
            .map(|dir| dir.join(exe))
            .find(|candidate| candidate.is_file())
    }

    fn find_windows_sdk_rc() -> Option<PathBuf> {
        let arch_dir = match std::env::var("TARGET").ok()?.as_str() {
            target if target.contains("aarch64") => "arm64",
            target if target.contains("i686") => "x86",
            _ => "x64",
        };

        let mut candidates = Vec::new();
        for root_var in ["ProgramFiles(x86)", "ProgramFiles"] {
            let Some(root) = std::env::var_os(root_var) else {
                continue;
            };
            let bin_dir = Path::new(&root).join("Windows Kits").join("10").join("bin");
            let Ok(entries) = std::fs::read_dir(bin_dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let candidate = entry.path().join(arch_dir).join("rc.exe");
                if candidate.is_file() {
                    candidates.push(candidate);
                }
            }
        }

        candidates.sort();
        candidates.pop()
    }
}
