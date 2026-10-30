//! winuxcmd integration via PATH injection.
//!
//! rubash's `Executor` looks up external commands via `find_user_command()`,
//! which walks `PATH`. PATH integration is the default, the fallback, and the
//! equivalence reference: we prepend the directory containing `winuxcmd.exe`
//! to the process `PATH` so rubash finds it first. The opt-in FFI
//! acceleration surface (niubash#567) lives in [`crate::ffi`]; its discovery
//! starts from the installation root this module's search establishes.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};

/// Locate `winuxcmd.exe` by checking, in order:
///   1. `$WINUXCMD_PATH` env var (file or directory)
///   2. `<exe_dir>/usr/bin/winuxcmd.exe`
///   3. `<exe_dir>/winuxcmd/usr/bin/winuxcmd.exe`
///   4. `<exe_dir>/utils/winuxcmd/usr/bin/winuxcmd.exe`
///   5. legacy flat locations and `winuxcmd.exe` on `PATH`
pub fn find_winuxcmd() -> Option<PathBuf> {
    find_winuxcmd_with_report().found
}

#[derive(Debug)]
struct WinuxCmdSearchReport {
    found: Option<PathBuf>,
    /// Installation root implied by the candidate that matched. A candidate
    /// knows the layout it describes, so nothing has to probe the filesystem:
    /// `<root>\usr\bin\winuxcmd.exe`, `<root>\winuxcmd.exe` and
    /// `<root>\bin\winuxcmd.exe` (the PATH entry link) all imply their own
    /// root, while `<x>\winuxcmd\usr\bin\winuxcmd.exe` implies
    /// `<x>\winuxcmd`.
    root: Option<PathBuf>,
    checked: Vec<String>,
}

fn find_winuxcmd_with_report() -> WinuxCmdSearchReport {
    let override_path = std::env::var("WINUXCMD_PATH").ok().map(PathBuf::from);
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf));

    find_winuxcmd_with_report_from_sources(
        override_path.as_deref(),
        exe_dir.as_deref(),
        find_winuxcmd_on_path,
    )
}

fn find_winuxcmd_with_report_from_sources<F>(
    override_path: Option<&Path>,
    exe_dir: Option<&Path>,
    path_lookup: F,
) -> WinuxCmdSearchReport
where
    F: FnOnce(&mut Vec<String>) -> Option<PathBuf>,
{
    let mut checked = Vec::new();

    // 1. WINUXCMD_PATH override
    if let Some(path) = override_path {
        checked.extend(override_search_descriptions(path));
        if let Some(exe) = resolve_winuxcmd_override(path) {
            return WinuxCmdSearchReport {
                found: Some(exe),
                root: None,
                checked,
            };
        }
    }

    // 2/3/4. Relative to current executable
    if let Some(exe_dir) = exe_dir {
        for candidate in bundled_winuxcmd_candidates(exe_dir) {
            checked.push(candidate.path.display().to_string());
            if candidate.path.is_file() {
                return WinuxCmdSearchReport {
                    found: Some(candidate.path),
                    root: Some(candidate.root),
                    checked,
                };
            }
        }
    }

    // 5. PATH lookup with an in-process walk on Windows (no child: a
    // `where.exe` spawn here would claim a hidden session console on
    // console-less hosts — see find_winuxcmd_on_path)
    if let Some(exe) = path_lookup(&mut checked) {
        return WinuxCmdSearchReport {
            found: Some(exe),
            root: None,
            checked,
        };
    }

    WinuxCmdSearchReport {
        found: None,
        root: None,
        checked,
    }
}

fn find_winuxcmd_on_path(checked: &mut Vec<String>) -> Option<PathBuf> {
    #[cfg(windows)]
    {
        // niubash#451/#462: this probe runs on EVERY shell startup
        // (`Shell::new*` -> `prepare_winuxcmd_with_override`), including
        // console-less `niu -c` sessions that never execute an external
        // command. Spawning `where.exe` here used to claim the session's
        // hidden console (via `suppress_orphan_console`'s
        // `try_alloc_hidden_console`) — or burn its own transient conhost —
        // so a pure-builtin session allocated 1 conhost.exe and tripped the
        // `detached_builtin_only_session_allocates_zero_conhosts` gate.
        // `where.exe winuxcmd.exe` is just an ordered file-existence walk,
        // so do it in-process: same result, no child, no console, no
        // ~30 ms spawn tax.
        checked.push("PATH lookup for winuxcmd.exe (in-process walk)".to_string());
        let exe = find_file_in_path_dirs("winuxcmd.exe");
        if let Some(path) = exe.as_ref() {
            checked.push(path.display().to_string());
        }
        exe
    }

    #[cfg(not(windows))]
    {
        checked.push("PATH lookup for winuxcmd.exe is only available on Windows".to_string());
        None
    }
}

/// `where.exe <name>` without the child: current directory first, then PATH
/// in order (an empty entry names the current directory — `cmd.exe`
/// semantics, same as `where.exe`), first exact-name file wins. Pure
/// filesystem checks, so it is safe on a console-less host where a spawn
/// would claim a hidden session console (niubash#451/#462).
#[cfg(any(windows, test))]
fn find_file_in_path_dirs(file_name: &str) -> Option<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Ok(cwd) = std::env::current_dir() {
        dirs.push(cwd);
    }
    if let Some(path_var) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path_var) {
            if dir.as_os_str().is_empty() {
                if let Ok(cwd) = std::env::current_dir() {
                    dirs.push(cwd);
                }
            } else {
                dirs.push(dir);
            }
        }
    }
    dirs.into_iter()
        .map(|dir| dir.join(file_name))
        .find(|candidate| candidate.is_file())
}

/// Prepend the directory containing `winuxcmd.exe` to `PATH` so rubash's
/// command lookup finds winuxcmd-provided coreutils first. Returns the
/// directory that was injected, or an error if winuxcmd couldn't be found.
pub fn ensure_on_path() -> Result<PathBuf> {
    ensure_on_path_with_override(None)
}

/// Resolve the single WinuxCmd executable selected for this shell session.
///
/// Selection belongs to Niubash. Rubash receives the returned absolute path
/// and must not independently discover another dispatcher from PATH.
pub fn resolve_winuxcmd_with_override(override_path: Option<&Path>) -> Result<PathBuf> {
    Ok(find_winuxcmd_report_with_override(override_path)?
        .found
        .expect("checked above"))
}

/// The search report for an explicit override or the default discovery, with
/// the implied installation root when a bundled candidate matched.
fn find_winuxcmd_report_with_override(
    override_path: Option<&Path>,
) -> Result<WinuxCmdSearchReport> {
    match override_path {
        Some(path) => resolve_winuxcmd_override(path)
            .map(|exe| WinuxCmdSearchReport {
                found: Some(exe),
                // An override names a file or a directory, not a layout: the
                // root has to be derived from the path itself.
                root: None,
                checked: override_search_descriptions(path),
            })
            .ok_or_else(|| {
                anyhow!(
                    "configured winuxcmd path '{}' does not point to winuxcmd.exe or a containing directory; checked: {}",
                    path.display(),
                    format_checked_paths(&override_search_descriptions(path))
                )
            }),
        None => {
            let report = find_winuxcmd_with_report();
            if report.found.is_some() {
                return Ok(report);
            }
            Err(anyhow!(
                "winuxcmd.exe not found; checked: {}",
                format_checked_paths(&report.checked)
            ))
        }
    }
}

/// Resolve the selected executable, activate its command links if needed, and
/// prepend exactly its containing directory to the process PATH.
pub fn prepare_winuxcmd_with_override(override_path: Option<&Path>) -> Result<PathBuf> {
    let report = find_winuxcmd_report_with_override(override_path)?;
    let exe = report.found.clone().expect("checked above");
    let root = report
        .root
        .clone()
        .unwrap_or_else(|| installation_root(&exe));
    auto_activate_bundled_winuxcmd(&exe);
    prepend_root_dirs_to_path(&root, &exe)?;
    prepend_session_link_dirs(&exe);
    Ok(exe)
}

/// The installation root for the selected executable, preferring the root
/// implied by the layout that matched over re-deriving it from the path.
fn installation_root_for_report(report: &WinuxCmdSearchReport, exe: &Path) -> PathBuf {
    report
        .root
        .clone()
        .unwrap_or_else(|| installation_root(exe))
}

/// Same as `ensure_on_path`, but an explicit config override takes precedence.
/// The override may point either to `winuxcmd.exe` or to its containing dir.
pub fn ensure_on_path_with_override(override_path: Option<&Path>) -> Result<PathBuf> {
    let report = find_winuxcmd_report_with_override(override_path)?;
    let exe = report.found.clone().expect("checked above");
    let root = installation_root_for_report(&report, &exe);
    auto_activate_bundled_winuxcmd(&exe);
    prepend_root_dirs_to_path(&root, &exe)?;
    prepend_session_link_dirs(&exe);
    Ok(root.join("usr").join("bin"))
}

/// The root that owns this session's link family. A packaged (MSIX)
/// install keeps its own tree read-only, so command links materialize in
/// the per-package writable root — the same root `prepare_shell_root`
/// prepares (`NIU_ROOT`). Everywhere else links live in the installation
/// root beside `winuxcmd.exe` (niubash#495). Windows-only at compile
/// time: its only consumer is the activation path's `wpm links rebuild`
/// block, which is itself Windows-only.
#[cfg(windows)]
fn session_links_root(exe: &Path) -> PathBuf {
    crate::msix::session_writable_root().unwrap_or_else(|| installation_root(exe))
}

/// The directory carrying the session's command links: `<root>\usr\bin`
/// under MSIX, the executable's own directory otherwise (exactly the
/// location the unpackaged contract always used).
fn session_link_bin(exe: &Path) -> PathBuf {
    if let Some(root) = crate::msix::session_writable_root() {
        return root.join("usr").join("bin");
    }
    exe.parent().map(Path::to_path_buf).unwrap_or_default()
}

/// Return the real installation root for a selected WinuxCmd executable.
pub fn installation_root(exe: &Path) -> PathBuf {
    let Some(bin_dir) = exe.parent() else {
        return PathBuf::new();
    };
    if bin_dir.file_name().is_some_and(|name| name == "bin") {
        let parent = bin_dir.parent().unwrap_or(bin_dir);
        if parent.file_name().is_some_and(|name| name == "usr") {
            return parent.parent().unwrap_or(parent).to_path_buf();
        }
        if parent.file_name().is_some_and(|name| name == "local")
            && parent
                .parent()
                .and_then(Path::file_name)
                .is_some_and(|name| name == "usr")
        {
            return parent
                .parent()
                .and_then(Path::parent)
                .unwrap_or(parent)
                .to_path_buf();
        }
    }
    bin_dir.to_path_buf()
}

/// How far above the running executable the bundled tree may live. `niu.exe` is
/// hardlinked into `<root>\bin` (the PATH entry directory), into the winuxcmd
/// root's own `bin`, and - for the agent/editor entry points - one or two levels
/// beside the installation, so a handful of levels covers every supported
/// layout while keeping discovery deterministic and bounded.
const MAX_BUNDLED_ANCESTOR_LEVELS: usize = 6;

/// A WinuxCmd executable a bundled layout may contain, with the installation
/// root that layout implies.
#[derive(Debug, Clone, PartialEq, Eq)]
struct BundledCandidate {
    path: PathBuf,
    root: PathBuf,
}

fn bundled_winuxcmd_candidates(exe_dir: &Path) -> Vec<BundledCandidate> {
    // (relative path, installation root relative to the same base)
    const RELATIVE: [(&str, &str); 7] = [
        ("usr/bin/winuxcmd.exe", ""),
        ("winuxcmd.exe", ""),
        // The PATH entry link wpm creates: <root>\bin\winuxcmd.exe. Without
        // this shape a shell hardlinked beside it would resolve winuxcmd.exe
        // and then treat `bin` as the installation root, losing usr\bin.
        ("bin/winuxcmd.exe", ""),
        ("winuxcmd/usr/bin/winuxcmd.exe", "winuxcmd"),
        ("winuxcmd/winuxcmd.exe", "winuxcmd"),
        ("utils/winuxcmd/usr/bin/winuxcmd.exe", "utils/winuxcmd"),
        ("utils/winuxcmd/winuxcmd.exe", "utils/winuxcmd"),
    ];

    let mut candidates = Vec::new();
    let mut base = Some(exe_dir);
    let mut level = 0;
    while let Some(current) = base {
        for (rel, root_rel) in RELATIVE {
            candidates.push(BundledCandidate {
                path: current.join(rel),
                root: if root_rel.is_empty() {
                    current.to_path_buf()
                } else {
                    current.join(root_rel)
                },
            });
        }
        if level >= MAX_BUNDLED_ANCESTOR_LEVELS {
            break;
        }
        // `parent()` of a volume root is None, and of a relative dir like
        // "bundle" is Some(""): both end the walk.
        base = current
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty() && *parent != current);
        level += 1;
    }
    candidates
}

fn override_search_descriptions(path: &Path) -> Vec<String> {
    if path.is_dir() {
        vec![
            path.join("usr/bin/winuxcmd.exe").display().to_string(),
            path.join("winuxcmd.exe").display().to_string(),
        ]
    } else {
        vec![path.display().to_string()]
    }
}

fn format_checked_paths(paths: &[String]) -> String {
    if paths.is_empty() {
        "(none)".to_string()
    } else {
        paths.join(", ")
    }
}

fn resolve_winuxcmd_override(path: &Path) -> Option<PathBuf> {
    if path.is_file()
        && path
            .file_name()
            .and_then(|name| name.to_str())
            .map(|name| name.eq_ignore_ascii_case("winuxcmd.exe"))
            .unwrap_or(false)
    {
        return Some(path.to_path_buf());
    }

    if path.is_dir() {
        for relative in ["usr/bin/winuxcmd.exe", "winuxcmd.exe"] {
            let candidate = path.join(relative);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }

    None
}

/// Directories one installation contributes to PATH, in POSIX order. A tree
/// whose executable sits below the root injects all four; a portable layout
/// that keeps `winuxcmd.exe` next to its tools injects only that directory.
fn injected_path_dirs(root: &Path, exe: &Path) -> Vec<PathBuf> {
    let exe_dir = exe.parent().map(Path::to_path_buf).unwrap_or_default();
    if root.as_os_str().is_empty() || root == exe_dir {
        return vec![exe_dir];
    }
    vec![
        root.join("usr/local/bin"),
        root.join("usr/bin"),
        root.join("bin"),
        root.to_path_buf(),
    ]
}

fn prepend_root_dirs_to_path(root: &Path, exe: &Path) -> Result<PathBuf> {
    let exe_dir = exe
        .parent()
        .ok_or_else(|| anyhow!("winuxcmd.exe has no parent directory"))?
        .to_path_buf();
    let dirs = injected_path_dirs(root, exe);

    let current_path = std::env::var("PATH").unwrap_or_default();
    let new_path = path_with_dirs_prepended(&current_path, &dirs);
    if new_path == current_path {
        return Ok(exe_dir);
    }
    // On Windows, `std::env::set_var` normalizes "PATH" to "Path".
    // Rubash internally uses `env_vars.get("PATH")` (all caps), which
    // is case-sensitive in the HashMap. Force the all-caps entry so rubash
    // can find it.
    #[cfg(windows)]
    std::env::set_var("PATH", &new_path);
    #[cfg(not(windows))]
    std::env::set_var("PATH", &new_path);
    log::debug!("winuxcmd PATH injected from: {}", root.display());
    Ok(exe_dir)
}

/// Under MSIX the link family sits in the writable per-package root, not
/// the read-only install tree. Its PATH dirs go ahead of the package's —
/// session-resolved applets and wpm-managed shims win — without stripping
/// the package dirs the flat payloads (git.exe, rg.exe, …) still ride in
/// (niubash#495). Non-packaged installs are a no-op here.
fn prepend_session_link_dirs(exe: &Path) {
    let Some(links_root) = crate::msix::session_writable_root() else {
        return;
    };
    let dirs = injected_path_dirs(&links_root, exe);
    let current_path = std::env::var("PATH").unwrap_or_default();
    let new_path = path_with_dirs_prepended_impl(&current_path, &dirs, false);
    if new_path == current_path {
        return;
    }
    std::env::set_var("PATH", &new_path);
    log::debug!("msix session PATH injected from: {}", links_root.display());
}

fn path_with_dirs_prepended(current_path: &str, dirs: &[PathBuf]) -> String {
    path_with_dirs_prepended_impl(current_path, dirs, true)
}

fn path_with_dirs_prepended_impl(
    current_path: &str,
    dirs: &[PathBuf],
    strip_winuxcmd_entries: bool,
) -> String {
    let desired = dirs
        .iter()
        .map(|dir| comparable_path_entry(&dir.to_string_lossy()))
        .collect::<Vec<_>>();
    let mut parts: Vec<String> = current_path
        .split(path_list_separator())
        .filter_map(|entry| {
            let entry = entry.trim();
            let comparable = comparable_path_entry(entry);
            (!entry.is_empty()
                && !desired.iter().any(|dir| dir == &comparable)
                && !(strip_winuxcmd_entries && is_winuxcmd_installation_entry(entry)))
            .then(|| entry.to_string())
        })
        .collect();
    for dir in dirs.iter().rev() {
        parts.insert(0, dir.to_string_lossy().to_string());
    }
    parts.join(&path_list_separator().to_string())
}

#[cfg(test)]
fn path_with_dir_prepended(current_path: &str, dir: &str) -> String {
    let mut parts: Vec<String> = current_path
        .split(path_list_separator())
        .filter_map(|entry| {
            let entry = entry.trim();
            (!entry.is_empty()
                && !path_entries_equal(entry, dir)
                && !is_winuxcmd_installation_entry(entry))
            .then(|| entry.to_string())
        })
        .collect();
    parts.insert(0, dir.to_string());
    parts.join(&path_list_separator().to_string())
}

fn is_winuxcmd_installation_entry(entry: &str) -> bool {
    #[cfg(windows)]
    {
        let path = PathBuf::from(entry.trim_matches('"'));
        path.join("winuxcmd.exe").is_file() || path.join("usr/bin/winuxcmd.exe").is_file()
    }

    #[cfg(not(windows))]
    {
        let _ = entry;
        false
    }
}

#[cfg(test)]
fn path_starts_with_dir(current_path: &str, dir: &str) -> bool {
    current_path
        .split(path_list_separator())
        .next()
        .map(|entry| path_entries_equal(entry.trim(), dir))
        .unwrap_or(false)
}

#[cfg(test)]
fn path_entries_equal(left: &str, right: &str) -> bool {
    comparable_path_entry(left) == comparable_path_entry(right)
}

fn comparable_path_entry(value: &str) -> String {
    let mut normalized = value.trim_matches('"').replace('\\', "/");
    while normalized.len() > 3 && normalized.ends_with('/') {
        normalized.pop();
    }
    if cfg!(windows) {
        normalized.to_ascii_lowercase()
    } else {
        normalized
    }
}

fn path_list_separator() -> char {
    if cfg!(windows) {
        ';'
    } else {
        ':'
    }
}

fn auto_activate_bundled_winuxcmd(exe: &Path) {
    if std::env::var_os("NIU_SKIP_WINUXCMD_ACTIVATION").is_some() {
        return;
    }

    let link_bin = session_link_bin(exe);
    if link_bin.as_os_str().is_empty() {
        return;
    }

    if has_required_command_links(&link_bin, exe) {
        return;
    }

    // Preferred path: run the exact command the Inno Setup installer uses
    // during [Run] (`wpm links rebuild --root <root> --force`). This keeps
    // portable and installed layouts on one activation path and no longer
    // depends on the bundled activation script sitting next to the
    // executable. Output is captured so non-interactive `-c`/`-C` runs stay
    // quiet and deterministic. Windows-only at compile time: `wpm` is a
    // Windows-only channel (the string must not surface in non-Windows
    // builds, and winuxcmd.exe is a Windows binary anyway).
    #[cfg(windows)]
    {
        let install_root = installation_root(exe);
        // niubash#495: under MSIX the install tree is read-only; the link
        // family lands in the per-package writable root instead.
        let links_root = session_links_root(exe);
        if links_root != install_root {
            prepare_packaged_links_root(&links_root, &install_root, exe);
        }
        match crate::console_guard::suppressed_command(exe)
            .args(["wpm", "links", "rebuild", "--root"])
            .arg(&links_root)
            .arg("--force")
            .output()
        {
            Ok(output) if output.status.success() => {
                log::debug!(
                    "winuxcmd activation completed via wpm links rebuild (root {})",
                    links_root.display()
                );
            }
            Ok(output) => {
                log::debug!(
                    "wpm links rebuild failed with status {}; falling back to activation script",
                    output.status
                );
            }
            Err(err) => {
                log::debug!(
                    "failed to launch wpm links rebuild: {err}; falling back to activation script"
                );
            }
        }
        // niubash#468: the bundled package shims (`usr\bin\awk.exe`,
        // `gawk.exe`) sit outside wpm's reconcile set on a state-free
        // install, so a successful `links rebuild` can still leave them on
        // the previous winuxcmd.exe image. Heal them in-process before
        // reporting the set complete.
        let shims = reconcile_bundled_shims(&links_root, true);
        if !shims.failed.is_empty() {
            log::debug!(
                "bundled shim reconcile left {} name(s) unresolved",
                shims.failed.len()
            );
        }
        if has_required_command_links(&link_bin, exe) {
            return;
        }
        log::debug!(
            "expected links are still missing in {}; falling back to activation script",
            link_bin.display()
        );
    }

    // niubash#497: a packaged install keeps the bundled script beside the
    // read-only package executable, and it writes the package root — under
    // MSIX it can only burn seconds and exit 1. The `wpm links rebuild`
    // plus the in-process reconcile above are the whole activation there.
    if crate::msix::is_packaged() {
        return;
    }

    // Fallback for older WinuxCmd builds without the `wpm` subcommand: run
    // the bundled activation script through this shell.
    let Some(script) = bundled_activation_script(exe) else {
        return;
    };

    let current_exe = match std::env::current_exe() {
        Ok(path) => path,
        Err(err) => {
            log::warn!("failed to locate niubash for winuxcmd activation: {}", err);
            return;
        }
    };

    let script_name = script
        .file_name()
        .unwrap_or_else(|| std::ffi::OsStr::new("activate-winuxcmd.sh"));

    let Some(script_dir) = script.parent() else {
        return;
    };
    match crate::console_guard::suppressed_command(current_exe)
        .arg(script_name)
        .current_dir(script_dir)
        .env("NIU_SKIP_WINUXCMD_ACTIVATION", "1")
        .output()
    {
        Ok(output) if output.status.success() => {
            log::debug!("winuxcmd activation completed: {}", script.display());
        }
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            log::warn!(
                "winuxcmd activation failed with status {}: {}",
                output.status,
                stderr.trim()
            );
        }
        Err(err) => {
            log::warn!("failed to run winuxcmd activation script: {}", err);
        }
    }
}

fn bundled_activation_script(exe: &Path) -> Option<PathBuf> {
    let dir = exe.parent()?;
    let script = dir.join("activate-winuxcmd.sh");
    script.is_file().then_some(script)
}

fn has_required_command_links(bin_dir: &Path, engine: &Path) -> bool {
    // Presence is not enough: a command link left on the *previous*
    // winuxcmd.exe by an update runs an older build forever, and nothing else
    // notices (unixwin/WinuxCmd#1206 - observed as a stale `yes.exe` hours
    // after the update). Comparing identity makes activation - which runs
    // `wpm links rebuild --force` when this returns false - repair the set on
    // the next shell start instead of waiting for someone to look.
    //
    // The image the links bind to is the engine materialized *beside* them
    // when one exists (niubash#497): under MSIX the package's winuxcmd.exe
    // cannot be hardlinked into the writable root, so activation leaves a
    // copy there and `wpm links rebuild` links against that copy — a
    // different file identity carrying the same bytes. Comparing the links
    // against the resolved package `engine` instead would fail on every
    // session start and re-run an activation that can never converge. The
    // materialized copy must still BE the resolved image, though: a
    // package upgrade leaves a stale copy whose own link family would
    // otherwise pass.
    let materialized = bin_dir.join("winuxcmd.exe");
    let engine = if materialized.is_file() {
        if !super::shell_links::same_file(&materialized, engine)
            && !super::shell_links::same_file_bytes(&materialized, engine)
        {
            return false;
        }
        materialized
    } else {
        engine.to_path_buf()
    };
    let builtins_ready = ["ls.exe", "cat.exe", "grep.exe", "ln.exe"]
        .iter()
        .all(|name| {
            let link = bin_dir.join(name);
            link.is_file() && super::shell_links::same_file(&engine, &link)
        });
    builtins_ready && bundled_shim_links_current(bin_dir, &engine)
}

/// The bundled-shim half of `has_required_command_links` (niubash#468): a
/// declared package-shim name that is missing or no longer the winuxcmd
/// image counts as "activation needed", so a stale `awk.exe`/`gawk.exe`
/// heals on the next shell start the way the builtins heal.
///
/// `bin_dir` is the directory holding the session's `winuxcmd` links; the
/// bundled contract only exists for the `<root>\usr\bin` layout — a flat
/// portable layout or the `bin` PATH-entry directory declares nothing and
/// is always "current". Names the machine's own `.wpm` state claims are
/// wpm's problem, not ours.
fn bundled_shim_links_current(bin_dir: &Path, engine: &Path) -> bool {
    let Some(usr) = bin_dir.parent() else {
        return true;
    };
    if usr.file_name().is_none_or(|name| name != "usr") {
        return true;
    }
    let Some(root) = usr.parent() else {
        return true;
    };
    let forwarder = bin_dir.join("winuxcmd-shim.exe");
    for component in crate::setup_wizard::bundled_link_shim_components() {
        let opt = root.join("opt").join(component.opt_dir);
        for shim in component.shims {
            // A leg or machine without the payload declares nothing.
            if !opt.join(shim).is_file() {
                continue;
            }
            if wpm_declares_command(root, shim) {
                continue;
            }
            let target = bin_dir.join(shim);
            if !bundled_shim_is_current(&target, engine, &forwarder) {
                return false;
            }
        }
    }
    true
}

/// Whether `target` already carries the image it should: a real link to
/// `winuxcmd.exe` (or to the `winuxcmd-shim.exe` forwarder — the shape wpm
/// materializes on state-managed installs), or a byte-identical copy of
/// either, which is the documented fallback on file systems without links.
fn bundled_shim_is_current(target: &Path, engine: &Path, forwarder: &Path) -> bool {
    if !target.is_file() {
        return false;
    }
    for image in [engine, forwarder] {
        if !image.is_file() {
            continue;
        }
        if super::shell_links::same_file(image, target)
            || super::shell_links::same_file_bytes(image, target)
        {
            return true;
        }
    }
    false
}

/// Whether the machine's own `.wpm` state claims `usr\bin\<name>`: a shim
/// map entry (`<cmd>=<payload>`), or an install receipt listing that exact
/// path among its destinations. wpm-owned names heal through `wpm links
/// rebuild` and package upgrades, and the bundled reconcile must never
/// touch them — on a real machine `wpm install awk` is a *flat* package
/// whose `usr\bin\awk.exe` IS the GNU payload, not a shim at all
/// (niubash#468).
fn wpm_declares_command(root: &Path, name: &str) -> bool {
    let stem = name.strip_suffix(".exe").unwrap_or(name);
    let state = root.join(".wpm");

    // The forwarder map: one `<cmd>=<payload>` line per shim command.
    if let Ok(map) = std::fs::read_to_string(state.join("shims.map")) {
        let declared = map.lines().any(|line| {
            line.trim_start()
                .split('=')
                .next()
                .is_some_and(|cmd| cmd.trim().eq_ignore_ascii_case(stem))
        });
        if declared {
            return true;
        }
    }

    // Install receipts: a `usr\bin\<name>` destination means the file is the
    // package's own payload. Receipts store JSON-escaped Windows paths, so
    // the backslash form appears doubled in the raw text.
    let needle_escaped = format!("usr\\\\bin\\\\{name}").to_lowercase();
    let needle_forward = format!("usr/bin/{name}").to_lowercase();
    if let Ok(entries) = std::fs::read_dir(state.join("installed")) {
        for entry in entries.flatten() {
            if entry.path().extension().and_then(|ext| ext.to_str()) != Some("json") {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(entry.path()) else {
                continue;
            };
            let text = text.to_lowercase();
            if text.contains(&needle_escaped) || text.contains(&needle_forward) {
                return true;
            }
        }
    }
    false
}

/// Create `target` as a hardlink of `source`, copying the bytes where the
/// file system cannot link — the documented fallback shape for these shims
/// (`preinstall.json`: "copies on file systems without hardlink support").
fn materialize_link(source: &Path, target: &Path) -> Result<()> {
    match std::fs::hard_link(source, target) {
        Ok(()) => Ok(()),
        Err(link_error) => std::fs::copy(source, target).map(|_| ()).with_context(|| {
            format!(
                "could not link or copy {} to {} ({link_error})",
                source.display(),
                target.display()
            )
        }),
    }
}

/// Materialize the pieces of the writable link root that
/// `wpm links rebuild --root <writable>` does not cover (niubash#495):
///
/// - `usr\bin\winuxcmd.exe` so the canonical engine the links bind to is
///   the package's image — and so a `winuxcmd`/`wpm` resolved through the
///   injected PATH derives its default root from its own module path,
///   which keeps `.wpm` receipts beside the links they manage;
/// - an `opt` view over the install's payload tree, so shim-layout
///   forwarding (`forwardToOptPayload` resolves `<root>\opt` from the
///   invoked link's own path) reaches the package payloads.
#[cfg(windows)]
fn prepare_packaged_links_root(links_root: &Path, install_root: &Path, exe: &Path) {
    let links_bin = links_root.join("usr").join("bin");
    if let Err(error) = std::fs::create_dir_all(&links_bin) {
        log::debug!(
            "msix links bin {} is not writable: {}",
            links_bin.display(),
            error
        );
        return;
    }
    let engine_link = links_bin.join("winuxcmd.exe");
    // niubash#497: a hardlink into the writable root cannot bind the
    // package image (WindowsApps refuses the link), so activation falls
    // back to a copy — and a byte-equal copy IS the current image, not a
    // reason to re-materialize (and re-verify every link) each session.
    if !super::shell_links::same_file(&engine_link, exe)
        && !super::shell_links::same_file_bytes(&engine_link, exe)
    {
        if let Err(error) = materialize_link(exe, &engine_link) {
            log::debug!(
                "msix engine link {} -> {} failed: {}",
                engine_link.display(),
                exe.display(),
                error
            );
        }
    }
    prepare_packaged_opt_view(links_root, install_root);
}

/// Whether `path` is a reparse point (junction, symlink) — checked with
/// `symlink_metadata` so the link itself is examined, not its target.
#[cfg(windows)]
fn is_reparse_point(path: &Path) -> bool {
    use std::os::windows::fs::MetadataExt;
    // FILE_ATTRIBUTE_REPARSE_POINT (0x400).
    std::fs::symlink_metadata(path)
        .map(|meta| meta.file_attributes() & 0x400 != 0)
        .unwrap_or(false)
}

/// Copy `source` into `target` recursively (plain files and directories;
/// payload trees ship no links worth preserving). Used for the payload
/// directories the writable root must own outright.
#[cfg(windows)]
fn copy_tree(source: &Path, target: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(target)?;
    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let destination = target.join(entry.file_name());
        if file_type.is_dir() {
            copy_tree(&entry.path(), &destination)?;
        } else if file_type.is_file() {
            std::fs::copy(entry.path(), &destination)?;
        }
    }
    Ok(())
}

/// The writable root's `opt` view over the read-only package payloads
/// (niubash#497). It is a real directory of per-package entries so both
/// shapes can coexist:
///
/// - forwarded (`layout: shim`) payloads land as real copies — the app
///   container denies a `CreateProcess` whose target reaches into the
///   package through a junction;
/// - every other package gets a per-package junction — the writable view
///   over read-only payloads the earlier single `opt` junction provided
///   (that shape is converted on the next activation).
///
/// Both forms self-heal: a junction left dangling by a package upgrade —
/// the install root is versioned, so an old target disappears — is
/// recreated, and a copied payload whose exes drifted from the package
/// bytes is recopied.
#[cfg(windows)]
fn prepare_packaged_opt_view(links_root: &Path, install_root: &Path) {
    let package_opt = install_root.join("opt");
    if !package_opt.is_dir() {
        return;
    }
    let opt = links_root.join("opt");
    // Convert the single-junction shape the first packaged builds wrote:
    // a reparse point at `opt` itself must go before the real directory
    // can hold per-package entries.
    if is_reparse_point(&opt) && std::fs::remove_dir(&opt).is_err() {
        log::debug!(
            "msix opt junction {} could not be replaced by the opt view",
            opt.display()
        );
        return;
    }
    if let Err(error) = std::fs::create_dir_all(&opt) {
        log::debug!("msix opt dir {} is not writable: {}", opt.display(), error);
        return;
    }
    let Ok(entries) = std::fs::read_dir(&package_opt) else {
        return;
    };
    for entry in entries.flatten() {
        if entry.file_type().map(|ty| !ty.is_dir()).unwrap_or(true) {
            continue;
        }
        let name = entry.file_name();
        let source = entry.path();
        let target = opt.join(&name);
        // A `layout: shim` payload's commands launch by `CreateProcess`
        // from the forwarder — inside a real app-container that launch is
        // denied when the payload path traverses a junction into
        // `C:\Program Files\WindowsApps` (error 5), while the same payload
        // copied under `LocalCache` runs — the same mechanism that lets
        // flat payload copies (curl, 7z) work (niubash#497). Only the
        // bundled shim set (gawk today) pays the copy; everything else
        // junctions.
        let component = crate::setup_wizard::bundled_link_shim_components()
            .find(|component| name == std::ffi::OsStr::new(component.opt_dir));
        match component {
            Some(component) => ensure_opt_payload_copy(component, &source, &target),
            None => ensure_opt_junction(&source, &target),
        }
    }
}

/// Keep `target` a live junction to `source`: absent names get created,
/// dangling junctions (the package moved under a new versioned install
/// root on upgrade) get recreated, and live junctions or real
/// directories — e.g. a `wpm install` payload landed in the writable
/// root itself — stay untouched.
#[cfg(windows)]
fn ensure_opt_junction(source: &Path, target: &Path) {
    if std::fs::symlink_metadata(target).is_ok() {
        // `is_dir` follows the junction: a live one resolves to its
        // package directory, a real directory answers for itself.
        if target.is_dir() {
            return;
        }
        if is_reparse_point(target) {
            let _ = std::fs::remove_dir(target);
        }
    }
    if let Err(error) = crate::msix::create_junction(target, source) {
        log::debug!(
            "msix opt junction {} -> {} failed: {}",
            target.display(),
            source.display(),
            error
        );
    }
}

/// Keep `target` a real copy of the `source` payload directory. The copy
/// counts as current while every executable the component declares is
/// byte-present; anything else (absent, a junction, stale bytes from the
/// previous package build) is replaced wholesale.
#[cfg(windows)]
fn ensure_opt_payload_copy(
    component: &crate::setup_wizard::BundledComponent,
    source: &Path,
    target: &Path,
) {
    let current = target.is_dir()
        && !is_reparse_point(target)
        && component
            .exes
            .iter()
            .all(|exe| super::shell_links::same_file_bytes(&source.join(exe), &target.join(exe)));
    if current {
        return;
    }
    if is_reparse_point(target) {
        let _ = std::fs::remove_dir(target);
    } else if target.is_dir() {
        let _ = std::fs::remove_dir_all(target);
    } else if target.exists() {
        let _ = std::fs::remove_file(target);
    }
    if let Err(error) = copy_tree(source, target) {
        log::debug!(
            "msix opt payload copy {} -> {} failed: {}",
            source.display(),
            target.display(),
            error
        );
    }
}

/// Rebuild the bundled `layout: shim` command links under `<root>\usr\bin`
/// (niubash#468).
///
/// `wpm links rebuild` only reconciles names it can enumerate — the builtin
/// applet set plus package shims recorded in `.wpm` — and a bundled install
/// ships no `.wpm` state, so `usr\bin\gawk.exe` / `awk.exe` fall outside
/// every wpm reconcile. An upgrade that replaces `winuxcmd.exe` under its
/// own name leaves those links on the previous image's inode: same name,
/// old bytes, invisible to `wpm links verify` — exactly the 1.4.1 stale
/// `awk.exe`/`gawk.exe` residue. This is the matching reconcile for the
/// bundled set, driven by the same `BUNDLED_COMPONENTS` table `niu doctor`
/// reports from:
///
/// - a missing declared name is materialized as a `winuxcmd.exe` link
///   (payload-present names only — a leg without `opt\<pkg>` declares none);
/// - a name already on the current image — real link or byte-identical
///   copy, `winuxcmd.exe` or the `winuxcmd-shim.exe` forwarder — is left;
/// - a byte-equal copy is upgraded to a real hardlink so the on-disk shape
///   matches what `wpm install` produces;
/// - anything else is stale residue: parked and relinked under `--force`,
///   reported and left otherwise;
/// - a name the machine's own `.wpm` state declares is never touched.
pub fn reconcile_bundled_shims(root: &Path, force: bool) -> crate::shell_links::ShellLinkReport {
    let mut report = crate::shell_links::ShellLinkReport::default();
    let bin = root.join("usr").join("bin");
    let engine = bin.join("winuxcmd.exe");
    let forwarder = bin.join("winuxcmd-shim.exe");

    for component in crate::setup_wizard::bundled_link_shim_components() {
        let opt = root.join("opt").join(component.opt_dir);
        for shim in component.shims {
            let target = bin.join(shim);
            let payload = opt.join(shim);
            if !payload.is_file() {
                if target.is_file() {
                    // Not ours: with no bundled payload this name belongs to
                    // something else (a user package, a manual file).
                    continue;
                }
                report.skipped.push((
                    target,
                    format!("payload {} is not installed on this leg", payload.display()),
                ));
                continue;
            }
            if wpm_declares_command(root, shim) {
                report
                    .skipped
                    .push((target, "declared by the machine's .wpm state".to_string()));
                continue;
            }
            if !engine.is_file() {
                report.failed.push((
                    target,
                    format!("{} is missing; nothing to link", engine.display()),
                ));
                continue;
            }
            if target.is_dir() {
                report
                    .failed
                    .push((target, "is a directory; refusing to replace".to_string()));
                continue;
            }
            if !target.exists() {
                match materialize_link(&engine, &target) {
                    Ok(()) => report.created.push(target),
                    Err(error) => report.failed.push((target, error.to_string())),
                }
                continue;
            }
            if super::shell_links::same_file(&engine, &target)
                || (forwarder.is_file() && super::shell_links::same_file(&forwarder, &target))
            {
                report.already_current.push(target);
                continue;
            }
            // A byte-identical copy is the right image in the fallback
            // shape; prefer the real hardlink where linking works so the
            // on-disk state matches `wpm install` and staleness stays
            // visible to identity checks.
            let identical = [&engine, &forwarder].into_iter().find(|image| {
                image.is_file() && super::shell_links::same_file_bytes(image, &target)
            });
            if let Some(image) = identical {
                match crate::shell_links::replace_with_link(image, &target) {
                    Ok(()) => report.created.push(target),
                    Err(_) => report.already_current.push(target),
                }
                continue;
            }
            if !force {
                report.skipped.push((
                    target,
                    "exists and is not the bundled winuxcmd.exe image; pass --force to replace it"
                        .to_string(),
                ));
                continue;
            }
            match crate::shell_links::replace_with_link(&engine, &target) {
                Ok(()) => report.created.push(target),
                Err(error) => report.failed.push((target, error.to_string())),
            }
        }
    }
    report
}

/// True when the resolved winuxcmd install already has its command links
/// (`ls.exe`, `cat.exe`, …) in place. Used by the setup wizard's preflight
/// check; activation itself runs automatically at shell startup.
pub fn command_links_ready() -> bool {
    find_winuxcmd()
        .map(|exe| has_required_command_links(&session_link_bin(&exe), &exe))
        .unwrap_or(false)
}

/// Run `winuxcmd.exe --version` and return the first line of stdout.
pub fn version() -> Option<String> {
    let exe = find_winuxcmd()?;
    let out = crate::console_guard::suppressed_command(&exe)
        .arg("--version")
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    text.lines().next().map(|s| s.to_string())
}

/// List all commands provided by winuxcmd by scanning its directory for
/// sibling `*.exe` shims, or by invoking `winuxcmd.exe --list` if supported.
pub fn list_commands() -> Vec<String> {
    // Try `--list` first (forward-compat with future winuxcmd versions).
    if let Some(exe) = find_winuxcmd() {
        if let Ok(out) = crate::console_guard::suppressed_command(&exe)
            .arg("--list")
            .output()
        {
            if out.status.success() {
                let text = String::from_utf8_lossy(&out.stdout);
                let cmds: Vec<String> = text
                    .lines()
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
                if !cmds.is_empty() {
                    return cmds;
                }
            }
        }
    }

    // Fallback: scan the link directory for *.exe shims (under MSIX the
    // family lives in the writable root, not beside the package's
    // winuxcmd.exe — niubash#495).
    if let Some(exe) = find_winuxcmd() {
        let dir = session_link_bin(&exe);
        {
            if let Ok(entries) = std::fs::read_dir(&dir) {
                let mut cmds: Vec<String> = Vec::new();
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path
                        .extension()
                        .and_then(|e| e.to_str())
                        .map(|s| s.eq_ignore_ascii_case("exe"))
                        .unwrap_or(false)
                    {
                        if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                            if stem != "winuxcmd" {
                                cmds.push(stem.to_string());
                            }
                        }
                    }
                }
                cmds.sort();
                return cmds;
            }
        }
    }

    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn override_can_point_to_exe_file() {
        let dir = unique_temp_dir("winuxcmd-file-override");
        std::fs::create_dir_all(&dir).unwrap();
        let exe = dir.join("winuxcmd.exe");
        std::fs::write(&exe, b"").unwrap();

        assert_eq!(resolve_winuxcmd_override(&exe), Some(exe.clone()));

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn override_can_point_to_containing_dir() {
        let dir = unique_temp_dir("winuxcmd-dir-override");
        std::fs::create_dir_all(&dir).unwrap();
        let exe = dir.join("winuxcmd.exe");
        std::fs::write(&exe, b"").unwrap();

        assert_eq!(resolve_winuxcmd_override(&dir), Some(exe));

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn invalid_override_is_ignored_by_resolver() {
        let dir = unique_temp_dir("winuxcmd-invalid-override");
        std::fs::create_dir_all(&dir).unwrap();
        let other = dir.join("other.exe");
        std::fs::write(&other, b"").unwrap();

        assert_eq!(resolve_winuxcmd_override(&other), None);
        assert_eq!(resolve_winuxcmd_override(&dir), None);

        let _ = std::fs::remove_dir_all(dir);
    }

    /// RAII guard restoring the process-global PATH when the test ends.
    struct SavedPath(Option<std::ffi::OsString>);
    impl SavedPath {
        fn set(dirs: &[PathBuf]) -> Self {
            let saved = std::env::var_os("PATH");
            let joined = std::env::join_paths(dirs).unwrap();
            // `set_var` is process-global; the repo pins libtest to one
            // thread (`RUST_TEST_THREADS=1`), so no test races this.
            std::env::set_var("PATH", &joined);
            SavedPath(saved)
        }
    }
    impl Drop for SavedPath {
        fn drop(&mut self) {
            if let Some(ref value) = self.0 {
                std::env::set_var("PATH", value);
            } else {
                std::env::remove_var("PATH");
            }
        }
    }

    #[test]
    fn path_walk_finds_first_path_entry_in_order() {
        // In-process replacement for `where.exe <name>`: no child process,
        // so a console-less startup never claims a hidden session console
        // (niubash#451/#462 gate). Unique probe names keep the process
        // working directory out of the equation.
        let probe = "niu-path-walk-probe-a.exe";
        let empty_dir = unique_temp_dir("winuxcmd-walk-empty");
        let full_dir = unique_temp_dir("winuxcmd-walk-full");
        std::fs::create_dir_all(&empty_dir).unwrap();
        std::fs::create_dir_all(&full_dir).unwrap();
        std::fs::write(full_dir.join(probe), b"").unwrap();

        let missing = unique_temp_dir("winuxcmd-walk-missing");
        let _saved = SavedPath::set(&[missing.clone(), empty_dir.clone(), full_dir.clone()]);
        assert_eq!(
            find_file_in_path_dirs(probe),
            Some(full_dir.join(probe)),
            "first PATH entry holding the file must win"
        );

        let _ = std::fs::remove_dir_all(empty_dir);
        let _ = std::fs::remove_dir_all(full_dir);
    }

    #[test]
    fn path_walk_returns_none_when_absent() {
        let missing = unique_temp_dir("winuxcmd-walk-absent");
        let _saved = SavedPath::set(&[missing]);
        assert_eq!(
            find_file_in_path_dirs("niu-path-walk-probe-absent-xyz.exe"),
            None
        );
    }

    #[test]
    fn bundled_candidates_check_sibling_exe_first() {
        let exe_dir = PathBuf::from("bundle");
        let candidates = bundled_winuxcmd_candidates(&exe_dir);

        assert_eq!(candidates[0].path, exe_dir.join("usr/bin/winuxcmd.exe"));
        assert_eq!(candidates[1].path, exe_dir.join("winuxcmd.exe"));
        assert_eq!(candidates[2].path, exe_dir.join("bin/winuxcmd.exe"));
        assert_eq!(
            candidates[3].path,
            exe_dir.join("winuxcmd/usr/bin/winuxcmd.exe")
        );
        assert_eq!(candidates[0].root, exe_dir);
        assert_eq!(candidates[3].root, exe_dir.join("winuxcmd"));
    }

    #[test]
    fn bundled_candidates_climb_to_ancestor_installations() {
        // A shell hardlinked into the PATH entry directory finds the tree one
        // level up, and the shared root is what gets injected - not `bin`.
        let from_entry_bin = bundled_winuxcmd_candidates(Path::new(r"C:\tools\winuxcmd\bin"));
        let found = from_entry_bin
            .iter()
            .find(|candidate| candidate.path == *r"C:\tools\winuxcmd\usr\bin\winuxcmd.exe")
            .expect("the installation one level up must be a candidate");
        assert_eq!(found.root, PathBuf::from(r"C:\tools\winuxcmd"));

        // A package payload deep in the tree still reaches the bundled tree.
        let from_payload =
            bundled_winuxcmd_candidates(Path::new(r"C:\tools\winuxcmd\opt\niugit\bin"));
        assert!(from_payload
            .iter()
            .any(|candidate| candidate.path == *r"C:\tools\winuxcmd\usr\bin\winuxcmd.exe"));

        // The niubash layout: `<x>\bin\bash.exe` finds `<x>\winuxcmd\...`.
        let from_niubash_bin = bundled_winuxcmd_candidates(Path::new(r"C:\apps\Niubash\bin"));
        let bundled = from_niubash_bin
            .iter()
            .find(|candidate| candidate.path == *r"C:\apps\Niubash\winuxcmd\usr\bin\winuxcmd.exe")
            .expect("the bundled tree must be a candidate");
        assert_eq!(bundled.root, PathBuf::from(r"C:\apps\Niubash\winuxcmd"));
    }

    #[test]
    fn injected_path_dirs_cover_the_installation_root() {
        let root = Path::new(r"C:\tools\winuxcmd");
        let expected = vec![
            PathBuf::from(r"C:\tools\winuxcmd\usr/local/bin"),
            PathBuf::from(r"C:\tools\winuxcmd\usr/bin"),
            PathBuf::from(r"C:\tools\winuxcmd\bin"),
            PathBuf::from(r"C:\tools\winuxcmd"),
        ];
        // The canonical location, the PATH entry link (whose parent is `bin`),
        // and a copy that a package placed elsewhere in the tree all inject the
        // same four directories.
        for exe in [
            r"C:\tools\winuxcmd\usr\bin\winuxcmd.exe",
            r"C:\tools\winuxcmd\bin\winuxcmd.exe",
            r"C:\tools\winuxcmd\opt\pkg\bin\winuxcmd.exe",
        ] {
            assert_eq!(injected_path_dirs(root, Path::new(exe)), expected, "{exe}");
        }
        // A portable layout keeps the executable beside its tools: only that
        // directory is injected.
        assert_eq!(
            injected_path_dirs(
                Path::new(r"C:\portable"),
                Path::new(r"C:\portable\winuxcmd.exe")
            ),
            vec![PathBuf::from(r"C:\portable")]
        );
    }

    #[test]
    fn override_error_reports_concrete_checked_path() {
        let dir = unique_temp_dir("winuxcmd-empty-override");
        std::fs::create_dir_all(&dir).unwrap();

        let error = ensure_on_path_with_override(Some(&dir)).unwrap_err();
        let text = error.to_string();

        assert!(text.contains("checked:"), "{text}");
        assert!(
            text.contains(&dir.join("winuxcmd.exe").display().to_string()),
            "{text}"
        );

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn search_order_prefers_winuxcmd_path_override() {
        let override_dir = unique_temp_dir("winuxcmd-search-override");
        let bundle_dir = unique_temp_dir("winuxcmd-search-override-bundle");
        std::fs::create_dir_all(&override_dir).unwrap();
        std::fs::create_dir_all(&bundle_dir).unwrap();
        let override_exe = override_dir.join("winuxcmd.exe");
        let bundle_exe = bundle_dir.join("winuxcmd.exe");
        std::fs::write(&override_exe, b"").unwrap();
        std::fs::write(&bundle_exe, b"").unwrap();

        let report = find_winuxcmd_with_report_from_sources(
            Some(&override_dir),
            Some(&bundle_dir),
            |_checked| None,
        );

        assert_eq!(report.found, Some(override_exe));
        let _ = std::fs::remove_dir_all(override_dir);
        let _ = std::fs::remove_dir_all(bundle_dir);
    }

    #[test]
    fn search_order_finds_direct_sibling_bundle() {
        let exe_dir = unique_temp_dir("winuxcmd-search-sibling");
        std::fs::create_dir_all(&exe_dir).unwrap();
        let sibling = exe_dir.join("winuxcmd.exe");
        std::fs::write(&sibling, b"").unwrap();

        let report = find_winuxcmd_with_report_from_sources(None, Some(&exe_dir), |_checked| None);

        assert_eq!(report.found, Some(sibling));
        let _ = std::fs::remove_dir_all(exe_dir);
    }

    #[test]
    fn search_order_finds_nested_bundle_when_sibling_missing() {
        let exe_dir = unique_temp_dir("winuxcmd-search-nested");
        let nested_dir = exe_dir.join("winuxcmd");
        std::fs::create_dir_all(&nested_dir).unwrap();
        let nested = nested_dir.join("winuxcmd.exe");
        std::fs::write(&nested, b"").unwrap();

        let report = find_winuxcmd_with_report_from_sources(None, Some(&exe_dir), |_checked| None);

        assert_eq!(report.found, Some(nested));
        let _ = std::fs::remove_dir_all(exe_dir);
    }

    #[test]
    fn search_order_uses_path_fallback_after_bundle_locations() {
        let exe_dir = unique_temp_dir("winuxcmd-search-path");
        let path_dir = unique_temp_dir("winuxcmd-search-path-fallback");
        std::fs::create_dir_all(&exe_dir).unwrap();
        std::fs::create_dir_all(&path_dir).unwrap();
        let path_exe = path_dir.join("winuxcmd.exe");
        std::fs::write(&path_exe, b"").unwrap();

        let report = find_winuxcmd_with_report_from_sources(None, Some(&exe_dir), |checked| {
            checked.push(path_exe.display().to_string());
            Some(path_exe.clone())
        });

        assert_eq!(report.found, Some(path_exe));
        assert!(
            report
                .checked
                .iter()
                .any(|path| path.contains("utils") && path.ends_with("winuxcmd.exe")),
            "{:?}",
            report.checked
        );
        let _ = std::fs::remove_dir_all(exe_dir);
        let _ = std::fs::remove_dir_all(path_dir);
    }

    #[test]
    fn path_front_check_compares_complete_entries() {
        let sep = path_list_separator();
        let current = format!("C:/Tools/WinuxCmdExtra{sep}C:/Other");

        assert!(!path_starts_with_dir(&current, "C:/Tools/WinuxCmd"));
    }

    #[test]
    fn path_front_check_does_not_skip_leading_empty_entry() {
        let sep = path_list_separator();
        let current = format!("{sep}C:/Tools/WinuxCmd{sep}C:/Other");

        assert!(!path_starts_with_dir(&current, "C:/Tools/WinuxCmd"));
        assert_eq!(
            path_with_dir_prepended(&current, "C:/Tools/WinuxCmd"),
            format!("C:/Tools/WinuxCmd{sep}C:/Other")
        );
    }

    #[test]
    fn path_prepend_removes_duplicate_entries() {
        let sep = path_list_separator();
        let current = format!("C:/Other{sep}C:/Tools/WinuxCmd{sep}D:/Bin");
        let expected = format!("C:/Tools/WinuxCmd{sep}C:/Other{sep}D:/Bin");

        assert_eq!(
            path_with_dir_prepended(&current, "C:/Tools/WinuxCmd"),
            expected
        );
    }

    #[cfg(windows)]
    #[test]
    fn path_prepend_deduplicates_windows_equivalent_entries() {
        let current = "C:/Other;c:\\tools\\winuxcmd\\;D:/Bin";

        assert_eq!(
            path_with_dir_prepended(current, "C:/Tools/WinuxCmd"),
            "C:/Tools/WinuxCmd;C:/Other;D:/Bin"
        );
        assert!(path_starts_with_dir(
            "c:\\tools\\winuxcmd\\;C:/Other",
            "C:/Tools/WinuxCmd"
        ));
    }

    #[test]
    fn path_prepend_handles_empty_path() {
        assert_eq!(
            path_with_dir_prepended("", "C:/Tools/WinuxCmd"),
            "C:/Tools/WinuxCmd"
        );
    }

    #[test]
    fn bundled_activation_script_is_detected_next_to_winuxcmd() {
        let dir = unique_temp_dir("winuxcmd-activation-script");
        std::fs::create_dir_all(&dir).unwrap();
        let exe = dir.join("winuxcmd.exe");
        let script = dir.join("activate-winuxcmd.sh");
        std::fs::write(&exe, b"").unwrap();
        std::fs::write(&script, b"").unwrap();

        assert_eq!(bundled_activation_script(&exe), Some(script));

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn command_links_are_required_before_skipping_activation() {
        let dir = unique_temp_dir("winuxcmd-required-links");
        std::fs::create_dir_all(&dir).unwrap();
        let engine = dir.join("winuxcmd.exe");

        assert!(!has_required_command_links(&dir, &engine));

        // Names that are not links to this winuxcmd.exe do not count: an
        // update that left them on the previous image must trigger the rebuild.
        for name in ["ls.exe", "cat.exe", "grep.exe", "ln.exe"] {
            std::fs::write(dir.join(name), b"a different image").unwrap();
        }
        assert!(!has_required_command_links(&dir, &engine));

        std::fs::write(dir.join("winuxcmd.exe"), b"the installed engine").unwrap();
        for name in ["ls.exe", "cat.exe", "grep.exe", "ln.exe"] {
            let _ = std::fs::remove_file(dir.join(name));
            std::fs::hard_link(dir.join("winuxcmd.exe"), dir.join(name)).unwrap();
        }
        assert!(has_required_command_links(&dir, &engine));

        // One stale name is enough to ask for a rebuild.
        let stale = dir.join("cat.exe");
        let _ = std::fs::remove_file(&stale);
        std::fs::write(&stale, b"previous image").unwrap();
        assert!(!has_required_command_links(&dir, &engine));

        let _ = std::fs::remove_dir_all(dir);
    }

    /// A `<root>\usr\bin` layout with a fake engine image and, on request,
    /// the gawk payload the bundled shim set forwards to (niubash#468).
    fn fake_winuxcmd_root(tag: &str, with_gawk_payload: bool) -> PathBuf {
        let root = unique_temp_dir(tag);
        let bin = root.join("usr").join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(bin.join("winuxcmd.exe"), b"the installed engine").unwrap();
        for name in ["ls.exe", "cat.exe", "grep.exe", "ln.exe"] {
            std::fs::hard_link(bin.join("winuxcmd.exe"), bin.join(name)).unwrap();
        }
        if with_gawk_payload {
            let opt = root.join("opt").join("gawk");
            std::fs::create_dir_all(&opt).unwrap();
            std::fs::write(opt.join("gawk.exe"), b"gnu awk payload").unwrap();
            std::fs::write(opt.join("awk.exe"), b"gnu awk payload").unwrap();
        }
        root
    }

    #[test]
    fn reconcile_materializes_missing_bundled_shims() {
        let root = fake_winuxcmd_root("winuxcmd-shim-create", true);
        let bin = root.join("usr").join("bin");

        let report = reconcile_bundled_shims(&root, true);
        assert_eq!(report.created.len(), 2, "{report:?}");
        assert!(report.failed.is_empty(), "{report:?}");
        for name in ["gawk.exe", "awk.exe"] {
            assert!(
                crate::shell_links::same_file(&bin.join("winuxcmd.exe"), &bin.join(name)),
                "{} is not the engine image",
                bin.join(name).display()
            );
        }

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn reconcile_relinks_stale_copies_only_with_force() {
        let root = fake_winuxcmd_root("winuxcmd-shim-stale", true);
        let bin = root.join("usr").join("bin");
        // The 1.4.1 shape: a detached copy of the PREVIOUS winuxcmd.exe.
        let stale = bin.join("awk.exe");
        std::fs::write(&stale, b"the previous engine image").unwrap();
        let gawk = bin.join("gawk.exe");
        std::fs::write(&gawk, b"the previous engine image").unwrap();

        let kept = reconcile_bundled_shims(&root, false);
        assert_eq!(kept.skipped.len(), 2, "{kept:?}");
        assert!(kept.created.is_empty(), "{kept:?}");
        assert_eq!(std::fs::read(&stale).unwrap(), b"the previous engine image");

        let healed = reconcile_bundled_shims(&root, true);
        assert_eq!(healed.created.len(), 2, "{healed:?}");
        assert!(healed.failed.is_empty(), "{healed:?}");
        assert!(crate::shell_links::same_file(
            &bin.join("winuxcmd.exe"),
            &stale
        ));
        // The parking step must not leave `.niu-old-*` shapes behind.
        let leftovers: Vec<_> = std::fs::read_dir(&bin)
            .unwrap()
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name.contains(".niu-old-"))
            .collect();
        assert!(leftovers.is_empty(), "parked names left: {leftovers:?}");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn reconcile_upgrades_byte_equal_copies_to_links() {
        let root = fake_winuxcmd_root("winuxcmd-shim-copy", true);
        let bin = root.join("usr").join("bin");
        // The Inno/zip extraction shape: correct bytes, independent inode.
        std::fs::copy(bin.join("winuxcmd.exe"), bin.join("gawk.exe")).unwrap();
        std::fs::copy(bin.join("winuxcmd.exe"), bin.join("awk.exe")).unwrap();

        let report = reconcile_bundled_shims(&root, true);
        assert!(report.failed.is_empty(), "{report:?}");
        assert_eq!(
            report.created.len() + report.already_current.len(),
            2,
            "{report:?}"
        );
        for name in ["gawk.exe", "awk.exe"] {
            let target = bin.join(name);
            assert!(
                crate::shell_links::same_file(&bin.join("winuxcmd.exe"), &target)
                    || std::fs::read(&target).unwrap() == b"the installed engine",
                "{} carries neither the link nor the bytes",
                target.display()
            );
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn reconcile_never_touches_wpm_owned_names() {
        let root = fake_winuxcmd_root("winuxcmd-shim-wpm", true);
        let bin = root.join("usr").join("bin");
        let installed = root.join(".wpm").join("installed");
        std::fs::create_dir_all(&installed).unwrap();
        // The observed real-machine shape (niubash#468): a flat `wpm install
        // awk` whose receipt owns `usr\bin\awk.exe` — the real GNU binary,
        // not a shim. The reconcile must leave it alone.
        std::fs::write(
            installed.join("awk.json"),
            format!(
                "{{\"destinations\":[\"{}\"],\"layout\":\"flat\",\"name\":\"awk\"}}",
                bin.join("awk.exe")
                    .display()
                    .to_string()
                    .replace('\\', "\\\\")
            ),
        )
        .unwrap();
        let real_awk = bin.join("awk.exe");
        std::fs::write(&real_awk, b"the real gnu awk binary").unwrap();
        // ... and a forwarder-linked gawk.exe declared by the shim map.
        std::fs::write(bin.join("winuxcmd-shim.exe"), b"the forwarder").unwrap();
        std::fs::hard_link(bin.join("winuxcmd-shim.exe"), bin.join("gawk.exe")).unwrap();
        std::fs::write(
            root.join(".wpm").join("shims.map"),
            "gawk=opt\\gawk\\gawk.exe\n",
        )
        .unwrap();

        let report = reconcile_bundled_shims(&root, true);
        assert!(report.created.is_empty(), "{report:?}");
        assert!(report.failed.is_empty(), "{report:?}");
        assert_eq!(report.skipped.len(), 2, "{report:?}");
        assert_eq!(
            std::fs::read(&real_awk).unwrap(),
            b"the real gnu awk binary"
        );
        assert!(crate::shell_links::same_file(
            &bin.join("winuxcmd-shim.exe"),
            &bin.join("gawk.exe")
        ));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn reconcile_declares_nothing_without_the_payload() {
        // A leg without opt\gawk (arm64 ships no gawk): no shim is created,
        // and a foreign file at the name is left alone.
        let root = fake_winuxcmd_root("winuxcmd-shim-noleg", false);
        let bin = root.join("usr").join("bin");
        let foreign = bin.join("awk.exe");
        std::fs::write(&foreign, b"someone else's awk").unwrap();

        let report = reconcile_bundled_shims(&root, true);
        assert!(report.created.is_empty(), "{report:?}");
        assert!(report.failed.is_empty(), "{report:?}");
        assert_eq!(std::fs::read(&foreign).unwrap(), b"someone else's awk");

        let _ = std::fs::remove_dir_all(&root);
    }

    /// niubash#497: under MSIX the link family binds to the engine copy
    /// materialized beside the links — a different file identity carrying
    /// the package's bytes — not to the resolved package exe itself.
    /// Comparing links against the package path would reopen activation
    /// on every session.
    #[test]
    fn materialized_engine_copy_is_the_link_identity() {
        let base = unique_temp_dir("winuxcmd-msix-engine-copy");
        let pkg_bin = base.join("pkg").join("usr").join("bin");
        let links_bin = base.join("links").join("usr").join("bin");
        std::fs::create_dir_all(&pkg_bin).unwrap();
        std::fs::create_dir_all(&links_bin).unwrap();
        let package_engine = pkg_bin.join("winuxcmd.exe");
        std::fs::write(&package_engine, b"the packaged engine").unwrap();
        // Activation's copy fallback: same bytes, different file identity.
        let copy = links_bin.join("winuxcmd.exe");
        std::fs::copy(&package_engine, &copy).unwrap();
        for name in ["ls.exe", "cat.exe", "grep.exe", "ln.exe"] {
            std::fs::hard_link(&copy, links_bin.join(name)).unwrap();
        }

        assert!(has_required_command_links(&links_bin, &package_engine));

        // A package upgrade that changes the engine's bytes reopens
        // activation: the stale copy's own link family must not pass on
        // itself.
        std::fs::write(&package_engine, b"the upgraded engine").unwrap();
        assert!(!has_required_command_links(&links_bin, &package_engine));

        let _ = std::fs::remove_dir_all(&base);
    }

    /// niubash#497: the writable `opt` view carries forwarded
    /// (`layout: shim`) payloads as real copies — a CreateProcess through
    /// a junction into the package is denied inside a real app-container —
    /// and every other package as a per-package junction.
    #[cfg(windows)]
    #[test]
    fn packaged_opt_view_copies_shim_payloads_and_junctions_the_rest() {
        let base = unique_temp_dir("winuxcmd-msix-opt-view");
        let install = base.join("install");
        let install_opt = install.join("opt");
        let links_root = base.join("links");
        std::fs::create_dir_all(install_opt.join("gawk")).unwrap();
        std::fs::create_dir_all(install_opt.join("other")).unwrap();
        std::fs::write(install_opt.join("gawk").join("gawk.exe"), b"gnu awk").unwrap();
        std::fs::write(install_opt.join("gawk").join("awk.exe"), b"gnu awk").unwrap();
        std::fs::write(install_opt.join("other").join("tool.exe"), b"tool").unwrap();

        prepare_packaged_opt_view(&links_root, &install);

        let opt = links_root.join("opt");
        assert!(opt.is_dir());
        assert!(!is_reparse_point(&opt), "opt itself must be a real dir");

        let gawk = opt.join("gawk");
        assert!(gawk.is_dir());
        assert!(
            !is_reparse_point(&gawk),
            "forwarded payloads are real copies"
        );
        assert_eq!(std::fs::read(gawk.join("gawk.exe")).unwrap(), b"gnu awk");

        let other = opt.join("other");
        assert!(is_reparse_point(&other), "other packages stay junctions");
        assert!(other.join("tool.exe").is_file(), "junction resolves");

        // A second pass is a no-op, not a recopy; drifted bytes recopy.
        prepare_packaged_opt_view(&links_root, &install);
        std::fs::write(install_opt.join("gawk").join("gawk.exe"), b"new awk").unwrap();
        prepare_packaged_opt_view(&links_root, &install);
        assert_eq!(std::fs::read(gawk.join("gawk.exe")).unwrap(), b"new awk");

        // A junction left dangling by a moved install root recreates.
        let gone = base.join("gone");
        std::fs::create_dir_all(&gone).unwrap();
        std::fs::remove_dir(&other).unwrap();
        crate::msix::create_junction(&other, &gone).unwrap();
        std::fs::remove_dir_all(&gone).unwrap();
        prepare_packaged_opt_view(&links_root, &install);
        assert!(other.join("tool.exe").is_file());

        // A forwarded payload that somehow stayed a *live* junction — even
        // one whose bytes match — converts to a real copy: the junction is
        // the shape the app container denies at CreateProcess time.
        std::fs::remove_dir_all(&gawk).unwrap();
        crate::msix::create_junction(&gawk, &install_opt.join("gawk")).unwrap();
        prepare_packaged_opt_view(&links_root, &install);
        assert!(!is_reparse_point(&gawk));
        assert_eq!(std::fs::read(gawk.join("gawk.exe")).unwrap(), b"new awk");

        let _ = std::fs::remove_dir_all(&base);
    }

    /// The first packaged builds wrote `links_root\opt` as a single
    /// junction to the package tree; the per-package view replaces it.
    #[cfg(windows)]
    #[test]
    fn packaged_opt_view_replaces_the_legacy_single_junction() {
        let base = unique_temp_dir("winuxcmd-msix-opt-migrate");
        let install = base.join("install");
        let install_opt = install.join("opt");
        let links_root = base.join("links");
        std::fs::create_dir_all(install_opt.join("gawk")).unwrap();
        std::fs::write(install_opt.join("gawk").join("gawk.exe"), b"awk").unwrap();
        std::fs::write(install_opt.join("gawk").join("awk.exe"), b"awk").unwrap();
        crate::msix::create_junction(&links_root.join("opt"), &install_opt).unwrap();
        assert!(is_reparse_point(&links_root.join("opt")));

        prepare_packaged_opt_view(&links_root, &install);

        let opt = links_root.join("opt");
        assert!(opt.is_dir());
        assert!(!is_reparse_point(&opt));
        assert_eq!(
            std::fs::read(opt.join("gawk").join("gawk.exe")).unwrap(),
            b"awk"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn stale_bundled_shim_counts_as_activation_needed() {
        let root = fake_winuxcmd_root("winuxcmd-shim-gate", true);
        let bin = root.join("usr").join("bin");
        let engine = bin.join("winuxcmd.exe");
        std::fs::hard_link(&engine, bin.join("gawk.exe")).unwrap();
        std::fs::hard_link(&engine, bin.join("awk.exe")).unwrap();
        assert!(has_required_command_links(&bin, &engine));

        // A shim left on the previous image reopens activation.
        let stale = bin.join("awk.exe");
        std::fs::remove_file(&stale).unwrap();
        std::fs::write(&stale, b"the previous engine image").unwrap();
        assert!(!has_required_command_links(&bin, &engine));

        // ... and a missing name does too.
        std::fs::remove_file(&stale).unwrap();
        assert!(!has_required_command_links(&bin, &engine));

        let _ = std::fs::remove_dir_all(&root);
    }

    fn unique_temp_dir(prefix: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("{}-{}-{}", prefix, std::process::id(), nanos))
    }
}
