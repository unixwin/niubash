//! winuxcmd integration via PATH injection.
//!
//! rubash's `Executor` looks up external commands via `find_user_command()`,
//! which walks `PATH`. We don't use FFI/DLL -- we just prepend the directory
//! containing `winuxcmd.exe` to the process `PATH` so rubash finds it first.

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
        |checked| find_winuxcmd_on_path(checked),
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

    // 5. PATH lookup using `where.exe` on Windows
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
        checked.push("PATH lookup via where.exe winuxcmd.exe".to_string());
        if let Ok(out) = crate::console_guard::suppressed_command("where.exe")
            .arg("winuxcmd.exe")
            .output()
        {
            if out.status.success() {
                let text = String::from_utf8_lossy(&out.stdout);
                if let Some(line) = text.lines().next() {
                    let p = PathBuf::from(line.trim());
                    checked.push(p.display().to_string());
                    if p.is_file() {
                        return Some(p);
                    }
                }
            }
        }
    }

    #[cfg(not(windows))]
    {
        checked.push("PATH lookup for winuxcmd.exe is only available on Windows".to_string());
    }

    None
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
    Ok(root.join("usr").join("bin"))
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

fn path_with_dirs_prepended(current_path: &str, dirs: &[PathBuf]) -> String {
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
                && !is_winuxcmd_installation_entry(entry))
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
        return path.join("winuxcmd.exe").is_file() || path.join("usr/bin/winuxcmd.exe").is_file();
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

    let Some(dir) = exe.parent() else {
        return;
    };

    if has_required_command_links(dir) {
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
        let root = installation_root(exe);
        match crate::console_guard::suppressed_command(exe)
            .args(["wpm", "links", "rebuild", "--root"])
            .arg(&root)
            .arg("--force")
            .output()
        {
            Ok(output) if output.status.success() => {
                log::debug!(
                    "winuxcmd activation completed via wpm links rebuild (root {})",
                    root.display()
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
        let shims = reconcile_bundled_shims(&root, true);
        if !shims.failed.is_empty() {
            log::debug!(
                "bundled shim reconcile left {} name(s) unresolved",
                shims.failed.len()
            );
        }
        if has_required_command_links(dir) {
            return;
        }
        log::debug!(
            "expected links are still missing in {}; falling back to activation script",
            dir.display()
        );
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

    match crate::console_guard::suppressed_command(current_exe)
        .arg(script_name)
        .current_dir(dir)
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

fn has_required_command_links(dir: &Path) -> bool {
    // Presence is not enough: a command link left on the *previous*
    // winuxcmd.exe by an update runs an older build forever, and nothing else
    // notices (unixwin/WinuxCmd#1206 - observed as a stale `yes.exe` hours
    // after the update). Comparing identity makes activation - which runs
    // `wpm links rebuild --force` when this returns false - repair the set on
    // the next shell start instead of waiting for someone to look.
    let engine = dir.join("winuxcmd.exe");
    let builtins_ready = ["ls.exe", "cat.exe", "grep.exe", "ln.exe"]
        .iter()
        .all(|name| {
            let link = dir.join(name);
            link.is_file() && super::shell_links::same_file(&engine, &link)
        });
    builtins_ready && bundled_shim_links_current(dir)
}

/// The bundled-shim half of `has_required_command_links` (niubash#468): a
/// declared package-shim name that is missing or no longer the winuxcmd
/// image counts as "activation needed", so a stale `awk.exe`/`gawk.exe`
/// heals on the next shell start the way the builtins heal.
///
/// `dir` is the directory holding `winuxcmd.exe`; the bundled contract only
/// exists for the `<root>\usr\bin` layout — a flat portable layout or the
/// `bin` PATH-entry directory declares nothing and is always "current".
/// Names the machine's own `.wpm` state claims are wpm's problem, not ours.
fn bundled_shim_links_current(dir: &Path) -> bool {
    let Some(usr) = dir.parent() else {
        return true;
    };
    if usr.file_name().is_none_or(|name| name != "usr") {
        return true;
    }
    let Some(root) = usr.parent() else {
        return true;
    };
    let engine = dir.join("winuxcmd.exe");
    let forwarder = dir.join("winuxcmd-shim.exe");
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
            let target = dir.join(shim);
            if !bundled_shim_is_current(&target, &engine, &forwarder) {
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
        if super::shell_links::same_file(image, target) || same_file_bytes(image, target) {
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

/// Byte-for-byte equality for the copy-fallback shape: two paths that are
/// not the same file may still carry the same image (the Inno/zip payload
/// lays every bundled name down as a copy). Sizes first so the common
/// "different image" answer costs two stats instead of two 4 MB reads.
fn same_file_bytes(a: &Path, b: &Path) -> bool {
    let (Ok(a_meta), Ok(b_meta)) = (a.metadata(), b.metadata()) else {
        return false;
    };
    if a_meta.len() != b_meta.len() {
        return false;
    }
    let (Ok(a_bytes), Ok(b_bytes)) = (std::fs::read(a), std::fs::read(b)) else {
        return false;
    };
    a_bytes == b_bytes
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
            let identical = [&engine, &forwarder]
                .into_iter()
                .find(|image| image.is_file() && same_file_bytes(image, &target));
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
        .and_then(|exe| exe.parent().map(|dir| has_required_command_links(dir)))
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

    // Fallback: scan the directory for *.exe shims.
    if let Some(exe) = find_winuxcmd() {
        if let Some(dir) = exe.parent() {
            if let Ok(entries) = std::fs::read_dir(dir) {
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
            .find(|candidate| {
                candidate.path == PathBuf::from(r"C:\tools\winuxcmd\usr\bin\winuxcmd.exe")
            })
            .expect("the installation one level up must be a candidate");
        assert_eq!(found.root, PathBuf::from(r"C:\tools\winuxcmd"));

        // A package payload deep in the tree still reaches the bundled tree.
        let from_payload =
            bundled_winuxcmd_candidates(Path::new(r"C:\tools\winuxcmd\opt\niugit\bin"));
        assert!(from_payload
            .iter()
            .any(|candidate| candidate.path
                == PathBuf::from(r"C:\tools\winuxcmd\usr\bin\winuxcmd.exe")));

        // The niubash layout: `<x>\bin\bash.exe` finds `<x>\winuxcmd\...`.
        let from_niubash_bin = bundled_winuxcmd_candidates(Path::new(r"C:\apps\Niubash\bin"));
        let bundled = from_niubash_bin
            .iter()
            .find(|candidate| {
                candidate.path == PathBuf::from(r"C:\apps\Niubash\winuxcmd\usr\bin\winuxcmd.exe")
            })
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

        assert!(!has_required_command_links(&dir));

        // Names that are not links to this winuxcmd.exe do not count: an
        // update that left them on the previous image must trigger the rebuild.
        for name in ["ls.exe", "cat.exe", "grep.exe", "ln.exe"] {
            std::fs::write(dir.join(name), b"a different image").unwrap();
        }
        assert!(!has_required_command_links(&dir));

        std::fs::write(dir.join("winuxcmd.exe"), b"the installed engine").unwrap();
        for name in ["ls.exe", "cat.exe", "grep.exe", "ln.exe"] {
            let _ = std::fs::remove_file(dir.join(name));
            std::fs::hard_link(dir.join("winuxcmd.exe"), dir.join(name)).unwrap();
        }
        assert!(has_required_command_links(&dir));

        // One stale name is enough to ask for a rebuild.
        let stale = dir.join("cat.exe");
        let _ = std::fs::remove_file(&stale);
        std::fs::write(&stale, b"previous image").unwrap();
        assert!(!has_required_command_links(&dir));

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

    #[test]
    fn stale_bundled_shim_counts_as_activation_needed() {
        let root = fake_winuxcmd_root("winuxcmd-shim-gate", true);
        let bin = root.join("usr").join("bin");
        std::fs::hard_link(bin.join("winuxcmd.exe"), bin.join("gawk.exe")).unwrap();
        std::fs::hard_link(bin.join("winuxcmd.exe"), bin.join("awk.exe")).unwrap();
        assert!(has_required_command_links(&bin));

        // A shim left on the previous image reopens activation.
        let stale = bin.join("awk.exe");
        std::fs::remove_file(&stale).unwrap();
        std::fs::write(&stale, b"the previous engine image").unwrap();
        assert!(!has_required_command_links(&bin));

        // ... and a missing name does too.
        std::fs::remove_file(&stale).unwrap();
        assert!(!has_required_command_links(&bin));

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
