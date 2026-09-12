use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    emit_rubash_revision();

    #[cfg(windows)]
    embed_windows_icon();
}

/// Locate the `rubash` checkout that `Cargo.toml` pulls in as a path
/// dependency.
fn rubash_checkout_dir() -> Option<PathBuf> {
    let manifest_dir = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR")?);
    let sibling = manifest_dir.parent()?.join("rubash");
    sibling.is_dir().then_some(sibling)
}

/// Embed the rubash revision that is actually compiled into this binary.
///
/// `rubash` is a path dependency, so `Cargo.lock` carries no `source =` line
/// for it and the old lookup always fell through to the literal string
/// "master" — every niubash build printed the same revision no matter which
/// rubash commit was linked, and a release could not be traced back to its
/// engine.
fn emit_rubash_revision() {
    println!("cargo:rerun-if-changed=Cargo.lock");

    let revision = rubash_revision_from_checkout()
        .or_else(rubash_revision_from_lock)
        .unwrap_or_else(|| "unknown".to_string());

    println!("cargo:rustc-env=NIU_RUBASH_REV={revision}");
}

/// Ask the sibling checkout for the commit being compiled, marking it dirty
/// when the tree carries uncommitted changes. The dirty marker matters more
/// than the hash here: a tree with uncommitted edits is not reproducible, and
/// the version banner is the only place a user can see that.
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

    if let Some(status) = git(&dir, &["status", "--porcelain"]) {
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
/// sibling checkout's refs move, so watch them by hand. `HEAD` alone is not
/// enough — on a branch it stays `ref: refs/heads/<branch>` and only the
/// pointed-at ref file changes.
fn watch_rubash_refs(dir: &Path) {
    let dot_git = git_common_dir(dir).unwrap_or_else(|| dir.join(".git"));
    for rel in ["HEAD", "FETCH_HEAD", "refs/heads/master", "refs/heads/main"] {
        let path = dot_git.join(rel);
        if path.is_file() {
            println!("cargo:rerun-if-changed={}", path.display());
        }
    }
}

/// A submodule worktree stores `.git` as a file pointing at the superproject's
/// `modules/` directory; resolve it so the ref files can be watched.
fn git_common_dir(dir: &Path) -> Option<PathBuf> {
    let git_marker = dir.join(".git");
    if !git_marker.is_file() {
        return None;
    }
    let target = fs::read_to_string(&git_marker)
        .ok()
        .map(|text| text.trim().to_string())?;
    let rest = target.strip_prefix("gitdir:").map(str::trim)?;
    if rest.is_empty() {
        return None;
    }
    let absolute = PathBuf::from(rest);
    Some(if absolute.is_absolute() {
        absolute
    } else {
        dir.join(absolute)
    })
}

/// `git = "..."` dependencies record their revision in `Cargo.lock`; a path
/// dependency does not. Kept so the value stays correct if rubash ever moves
/// back to a git dependency.
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
            return source
                .rsplit_once('#')
                .map(|(_, rev)| rev.to_string())
                .or_else(|| Some("unknown".to_string()));
        }
    }

    None
}

#[cfg(windows)]
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
