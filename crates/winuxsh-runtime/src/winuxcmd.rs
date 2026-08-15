//! winuxcmd integration via PATH injection.
//!
//! rubash's `Executor` looks up external commands via `find_user_command()`,
//! which walks `PATH`. We don't use FFI/DLL -- we just prepend the directory
//! containing `winuxcmd.exe` to the process `PATH` so rubash finds it first.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{anyhow, Result};

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
                checked,
            };
        }
    }

    // 2/3/4. Relative to current executable
    if let Some(exe_dir) = exe_dir {
        for candidate in bundled_winuxcmd_candidates(exe_dir) {
            checked.push(candidate.display().to_string());
            if candidate.is_file() {
                return WinuxCmdSearchReport {
                    found: Some(candidate),
                    checked,
                };
            }
        }
    }

    // 5. PATH lookup using `where.exe` on Windows
    if let Some(exe) = path_lookup(&mut checked) {
        return WinuxCmdSearchReport {
            found: Some(exe),
            checked,
        };
    }

    WinuxCmdSearchReport {
        found: None,
        checked,
    }
}

fn find_winuxcmd_on_path(checked: &mut Vec<String>) -> Option<PathBuf> {
    #[cfg(windows)]
    {
        checked.push("PATH lookup via where.exe winuxcmd.exe".to_string());
        if let Ok(out) = Command::new("where.exe").arg("winuxcmd.exe").output() {
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
/// Selection belongs to Winuxsh. Rubash receives the returned absolute path
/// and must not independently discover another dispatcher from PATH.
pub fn resolve_winuxcmd_with_override(override_path: Option<&Path>) -> Result<PathBuf> {
    match override_path {
        Some(path) => resolve_winuxcmd_override(path).ok_or_else(|| {
            anyhow!(
                "configured winuxcmd path '{}' does not point to winuxcmd.exe or a containing directory; checked: {}",
                path.display(),
                format_checked_paths(&override_search_descriptions(path))
            )
        }),
        None => {
            let report = find_winuxcmd_with_report();
            report.found.ok_or_else(|| {
                anyhow!(
                    "winuxcmd.exe not found; checked: {}",
                    format_checked_paths(&report.checked)
                )
            })
        }
    }
}

/// Resolve the selected executable, activate its command links if needed, and
/// prepend exactly its containing directory to the process PATH.
pub fn prepare_winuxcmd_with_override(override_path: Option<&Path>) -> Result<PathBuf> {
    let exe = resolve_winuxcmd_with_override(override_path)?;
    auto_activate_bundled_winuxcmd(&exe);
    prepend_exe_dir_to_path(&exe)?;
    Ok(exe)
}

/// Same as `ensure_on_path`, but an explicit config override takes precedence.
/// The override may point either to `winuxcmd.exe` or to its containing dir.
pub fn ensure_on_path_with_override(override_path: Option<&Path>) -> Result<PathBuf> {
    let exe = prepare_winuxcmd_with_override(override_path)?;
    Ok(installation_root(&exe).join("usr").join("bin"))
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

fn bundled_winuxcmd_candidates(exe_dir: &Path) -> Vec<PathBuf> {
    [
        "usr/bin/winuxcmd.exe",
        "winuxcmd.exe",
        "winuxcmd/usr/bin/winuxcmd.exe",
        "winuxcmd/winuxcmd.exe",
        "utils/winuxcmd/usr/bin/winuxcmd.exe",
        "utils/winuxcmd/winuxcmd.exe",
    ]
    .into_iter()
    .map(|rel| exe_dir.join(rel))
    .collect()
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

fn prepend_exe_dir_to_path(exe: &Path) -> Result<PathBuf> {
    let root = installation_root(exe);
    let exe_dir = exe
        .parent()
        .ok_or_else(|| anyhow!("winuxcmd.exe has no parent directory"))?
        .to_path_buf();
    let dirs = if exe_dir.ends_with(Path::new("usr/bin")) {
        vec![
            root.join("usr/local/bin"),
            root.join("usr/bin"),
            root.join("bin"),
            root.clone(),
        ]
    } else {
        vec![exe_dir.clone()]
    };

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
        return path.join("winuxcmd.exe").is_file()
            || path.join("usr/bin/winuxcmd.exe").is_file();
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
    if std::env::var_os("WINUXSH_SKIP_WINUXCMD_ACTIVATION").is_some() {
        return;
    }

    let Some(script) = bundled_activation_script(exe) else {
        return;
    };
    let Some(dir) = exe.parent() else {
        return;
    };

    if has_required_command_links(dir) {
        return;
    }

    let current_exe = match std::env::current_exe() {
        Ok(path) => path,
        Err(err) => {
            log::warn!("failed to locate winuxsh for winuxcmd activation: {}", err);
            return;
        }
    };

    let script_name = script
        .file_name()
        .unwrap_or_else(|| std::ffi::OsStr::new("activate-winuxcmd.sh"));

    match Command::new(current_exe)
        .arg(script_name)
        .current_dir(dir)
        .env("WINUXSH_SKIP_WINUXCMD_ACTIVATION", "1")
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
    ["ls.exe", "cat.exe", "grep.exe", "ln.exe"]
        .iter()
        .all(|name| dir.join(name).is_file())
}

/// Run `winuxcmd.exe --version` and return the first line of stdout.
pub fn version() -> Option<String> {
    let exe = find_winuxcmd()?;
    let out = Command::new(&exe).arg("--version").output().ok()?;
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
        if let Ok(out) = Command::new(&exe).arg("--list").output() {
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

        assert_eq!(candidates[0], exe_dir.join("usr/bin/winuxcmd.exe"));
        assert_eq!(candidates[1], exe_dir.join("winuxcmd.exe"));
        assert_eq!(
            candidates[2],
            exe_dir.join("winuxcmd/usr/bin/winuxcmd.exe")
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

        for name in ["ls.exe", "cat.exe", "grep.exe", "ln.exe"] {
            std::fs::write(dir.join(name), b"").unwrap();
        }

        assert!(has_required_command_links(&dir));

        let _ = std::fs::remove_dir_all(dir);
    }

    fn unique_temp_dir(prefix: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("{}-{}-{}", prefix, std::process::id(), nanos))
    }
}
