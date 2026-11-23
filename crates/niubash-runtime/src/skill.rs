//! Agent skill bundle installation (`niu skill install` / `niu skill status`,
//! niubash#188).
//!
//! The bundle itself lives in `skills/niubash/` at the repo root and is
//! embedded by the launcher (src/skill.rs, packaging-correct: the files are
//! inside the root package, not this crate's). This module owns the
//! target-directory contract and the install/diff mechanics so they are
//! unit-testable without the embedded content:
//!
//! - Named targets mirror the WinuxCmd-skill precedent: one directory per
//!   agent host, each carrying `<skills-root>/niubash/SKILL.md` plus the
//!   bundle's `references/` and `agents/` files (same structure as the
//!   released `niubash-skill-v*.zip`, so agents can consume either).
//! - `--target <dir>` installs into `<dir>/niubash/` — the generic escape
//!   hatch for hosts this product does not name. Only path-shaped values
//!   (or the explicit `dir:<path>` form) are directories; an unknown bare
//!   word is an error, never a silent relative directory (niubash#579).
//!   A directory value spelled as a POSIX path (leading `/`) is resolved
//!   through the shell's own path classifier before it is used, so the
//!   spelling means what `cd` would mean by it from any cwd
//!   (niubash#1084).
//! - "Outdated" means drifted bytes, decided per file; there is no manifest
//!   file to get out of sync with the tree.

use std::fs;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

/// Directory name the bundle installs under, for every target.
pub const SKILL_DIR_NAME: &str = "niubash";

/// Named agent skill roots, in install/check order. Paths are relative to
/// the user home (the `dirs` home, consistent with path_utils).
pub const NAMED_TARGETS: &[(&str, &str)] = &[
    ("codex", ".codex/skills"),
    ("claude", ".claude/skills"),
    ("zcode", ".zcode/skills"),
    ("cursor", ".cursor/skills"),
];

/// One file of the bundle as the launcher embeds it.
pub struct SkillFile {
    /// Path relative to the bundle root (`SKILL.md`, `references/wpm.md`).
    pub rel: &'static str,
    /// Exact file content (LF endings; installs write it verbatim).
    pub content: &'static str,
}

/// Parsed `--target` value.
pub enum TargetSpec {
    /// A named host root from [`NAMED_TARGETS`].
    Named(&'static str),
    /// Every named host root.
    All,
    /// A generic skills root; installs `<dir>/niubash/`.
    Dir(PathBuf),
}

/// Parse a `--target` value: a named host, `all`, or a filesystem path.
///
/// The classification is by shape, not by per-word exceptions
/// (niubash#579): a bare word is a *name* and must be one of the named
/// hosts — an unknown bare word is an error that lists the legal names,
/// never a silent relative directory (a typo used to "install" the
/// bundle into `./<typo>/niubash/` under the current directory and
/// report success). A value is a directory when it is path-shaped —
/// it carries a path separator, is absolute, or starts with `./`,
/// `../` or `~` — or when it uses the explicit `dir:<path>` form,
/// which also covers bare directory names like `dir:skills`.
pub fn resolve_target(spec: &str) -> anyhow::Result<TargetSpec> {
    if spec == "all" {
        return Ok(TargetSpec::All);
    }
    if let Some((name, _)) = NAMED_TARGETS.iter().find(|(n, _)| *n == spec) {
        return Ok(TargetSpec::Named(name));
    }
    if spec.is_empty() {
        anyhow::bail!(
            "--target requires a name ({}, all) or a directory",
            named_list()
        );
    }
    if let Some(path) = spec.strip_prefix("dir:") {
        if path.is_empty() {
            anyhow::bail!("--target dir: requires a directory path after 'dir:'");
        }
        return Ok(TargetSpec::Dir(resolve_dir_path(path)?));
    }
    if is_path_shaped(spec) {
        return Ok(TargetSpec::Dir(resolve_dir_path(spec)?));
    }
    anyhow::bail!(
        "unknown --target '{spec}': not a named target ({}, all); \
         pass a directory path (one containing a path separator) or dir:<path> \
         for a custom skills root",
        named_list()
    )
}

/// Whether a `--target` value is shaped like a filesystem path rather
/// than a bare name: it contains a path separator (`/` or `\`), is
/// absolute, or opens with a `.`/`~` path component (`./`, `../`, `~`).
fn is_path_shaped(spec: &str) -> bool {
    spec.contains('/')
        || spec.contains('\\')
        || spec.starts_with('.')
        || spec.starts_with('~')
        || Path::new(spec).is_absolute()
}

/// Resolve a `--target` directory value to the host path it names.
///
/// Native Windows spellings (`C:/...`, `C:\...`, UNC) and relative
/// spellings keep the meaning they have always had here. A POSIX
/// spelling — one leading with `/` — is a shell dialect spelling, so
/// on Windows it goes through the shell's own path classifier
/// ([`Session::resolve_shell_path_from_env`], the one resolver the
/// engine's sessions resolve with), fed this process's environment
/// with the shell root configured the way the host publishes it.
/// The verbatim pass-through this replaces let Win32 read the leading
/// `/` as "root of the current drive": `dir:/tmp/x` landed on
/// whichever drive the process happened to sit on, and install and
/// status disagreed across drives (niubash#1084).
///
/// A leading-`/` spelling the classifier has no ruling for comes back
/// rooted-relative (rooted, but no drive — not absolute); that
/// residue is rejected outright instead of being silently
/// reinterpreted against the current drive.
fn resolve_dir_path(value: &str) -> anyhow::Result<PathBuf> {
    #[cfg(windows)]
    if value.starts_with('/') {
        let resolved =
            rubash::api::Session::resolve_shell_path_from_env(value, &target_env_snapshot());
        if resolved.is_absolute() {
            return Ok(resolved);
        }
        anyhow::bail!(
            "--target '{value}' leads with '/' but is not a shell path dialect \
             (drive form /c/..., /tmp, /var/tmp, /home, or a path under the \
             shell root); pass a native Windows path (e.g. C:/...) instead"
        );
    }
    Ok(PathBuf::from(value))
}

/// The process environment as the env map the shell path classifier
/// reads, with the shell root configured under a name the engine's
/// root lookup reads. The host publishes the root as `NIU_ROOT`
/// (`prepare_shell_root` takes it as the root, and
/// `publish_process_shell_root` writes it back, both in shell.rs);
/// the engine looks up `__RUBASH_SHELL_ROOT`, then `RUBASH_ROOT`, so
/// the published value is mirrored into the second name when neither
/// lookup name is already set. With no published root, the bundled
/// WinuxCmd installation root is the same fallback
/// `prepare_shell_root` itself uses. A rootless snapshot still
/// resolves every root-free dialect (the drive forms, /tmp, /var/tmp,
/// /home); only the install-tree spellings need the root.
#[cfg(windows)]
fn target_env_snapshot() -> std::collections::HashMap<String, String> {
    let mut env: std::collections::HashMap<String, String> = std::env::vars_os()
        .filter_map(|(name, value)| {
            Some((
                name.into_string().ok()?,
                value.to_string_lossy().into_owned(),
            ))
        })
        .collect();
    let root_configured = ["__RUBASH_SHELL_ROOT", "RUBASH_ROOT"]
        .iter()
        .any(|name| env.get(*name).is_some_and(|value| !value.is_empty()));
    if !root_configured {
        let root = env
            .get("NIU_ROOT")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .or_else(|| {
                crate::winuxcmd::find_winuxcmd().map(|exe| crate::winuxcmd::installation_root(&exe))
            });
        if let Some(root) = root {
            env.insert(
                "RUBASH_ROOT".to_string(),
                root.to_string_lossy().into_owned(),
            );
        }
    }
    env
}

/// Comma-separated named targets for usage text.
pub fn named_list() -> String {
    NAMED_TARGETS
        .iter()
        .map(|(name, _)| *name)
        .collect::<Vec<_>>()
        .join("|")
}

/// The install directory for a named host: `<home>/<skills-root>/niubash/`.
/// `None` when the home directory cannot be resolved.
pub fn named_target_dir(name: &str) -> Option<PathBuf> {
    let (_, rel) = NAMED_TARGETS.iter().find(|(n, _)| *n == name)?;
    Some(
        crate::path_utils::shell_home_dir()?
            .join(rel)
            .join(SKILL_DIR_NAME),
    )
}

/// Write every bundle file under `target_dir`, creating parent directories.
/// Existing files are overwritten (the bundle is the source of truth).
/// Returns the number of files written.
pub fn install_files(files: &[SkillFile], target_dir: &Path) -> anyhow::Result<usize> {
    for file in files {
        let path = target_dir.join(file.rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&path, file.content)?;
    }
    Ok(files.len())
}

/// Per-target install state, decided by byte comparison per file.
pub struct TargetStatus {
    /// The `.../niubash` directory this status describes.
    pub dir: PathBuf,
    /// All bundle files exist.
    pub installed: bool,
    /// Installed and every file matches the embedded content byte-for-byte.
    pub up_to_date: bool,
    /// Bundle files absent from the target.
    pub missing: Vec<&'static str>,
    /// Present but differing files (drifted or hand-edited).
    pub differing: Vec<&'static str>,
}

/// Compare a target directory against the bundle content.
pub fn check_target(files: &[SkillFile], dir: &Path) -> TargetStatus {
    let mut missing = Vec::new();
    let mut differing = Vec::new();
    for file in files {
        match fs::read(dir.join(file.rel)) {
            Ok(bytes) => {
                if bytes.as_slice() != file.content.as_bytes() {
                    differing.push(file.rel);
                }
            }
            Err(_) => missing.push(file.rel),
        }
    }
    TargetStatus {
        dir: dir.to_path_buf(),
        installed: missing.is_empty(),
        up_to_date: missing.is_empty() && differing.is_empty(),
        missing,
        differing,
    }
}

/// sha256 of the bundle's SKILL.md content, for `skill status` display —
/// lets a script tell two bundles apart without byte-comparing trees.
pub fn skill_digest(files: &[SkillFile]) -> Option<String> {
    let content = files
        .iter()
        .find(|file| file.rel == "SKILL.md")
        .map(|file| file.content)?;
    let digest = Sha256::digest(content.as_bytes());
    Some(
        digest
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>(),
    )
}

/// The bundle's own version, read from the generated `Bundle version`
/// line in SKILL.md (scripts/generate-skill.py stamps it from the root
/// package version). `skill status` prints it beside the digest so a
/// reader can tell *which* release a bundle is, not only *that* two
/// bundles differ (niubash#1083). Read from the bundle itself rather
/// than the binary's version: if the two ever disagree, the bundle's
/// own stamp is the one describing the bytes being installed.
pub fn bundle_version(files: &[SkillFile]) -> Option<String> {
    let content = files
        .iter()
        .find(|file| file.rel == "SKILL.md")
        .map(|file| file.content)?;
    const MARKER: &str = "**Bundle version**: niubash ";
    let start = content.find(MARKER)? + MARKER.len();
    let rest = &content[start..];
    let end = rest
        .find(|c: char| c.is_whitespace() || c == '(' || c == ')')
        .unwrap_or(rest.len());
    let version = &rest[..end];
    if version.is_empty() {
        None
    } else {
        Some(version.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FILES: &[SkillFile] = &[
        SkillFile {
            rel: "SKILL.md",
            content: "# bundle\n",
        },
        SkillFile {
            rel: "references/quickref.md",
            content: "quickref\n",
        },
    ];

    #[test]
    fn resolve_target_named_all_and_paths() {
        assert!(matches!(
            resolve_target("claude").unwrap(),
            TargetSpec::Named("claude")
        ));
        assert!(matches!(resolve_target("all").unwrap(), TargetSpec::All));
        assert!(matches!(
            resolve_target("C:/tmp/skills").unwrap(),
            TargetSpec::Dir(_)
        ));
        assert!(resolve_target("").is_err());
    }

    #[test]
    fn issue579_codex_is_a_named_target() {
        // Codex is the product's headline agent host; its skills root is
        // the vendor convention ~/.codex/skills.
        assert!(matches!(
            resolve_target("codex").unwrap(),
            TargetSpec::Named("codex")
        ));
        assert!(NAMED_TARGETS.contains(&("codex", ".codex/skills")));
    }

    #[test]
    fn issue579_unknown_bare_word_errors_and_lists_names() {
        // The regression shape: a typo'd name used to install silently
        // into ./<typo>/niubash under the current directory.
        for typo in ["cluade", "codexx", "skills", "agents"] {
            let err = match resolve_target(typo) {
                Ok(_) => panic!("{typo} must be rejected as an unknown bare word"),
                Err(e) => e.to_string(),
            };
            assert!(err.contains(typo), "{err}");
            assert!(err.contains("codex"), "{err}");
            assert!(err.contains("claude"), "{err}");
        }
    }

    #[test]
    fn issue579_path_shaped_and_explicit_dir_forms_stay_directories() {
        // "/abs/skills" is not in this list anymore: a leading-`/`
        // spelling is a shell dialect spelling since niubash#1084 and
        // resolves under the shell root — it is pinned, with the root
        // under the test's control, in
        // issue1084_root_spellings_resolve_under_the_shell_root.
        for spec in [
            "./skills",
            "../skills",
            "a/b",
            "a\\b",
            "C:\\skills",
            "~/skills",
            ".hidden/skills",
        ] {
            assert!(
                matches!(resolve_target(spec).unwrap(), TargetSpec::Dir(_)),
                "{spec}"
            );
        }
        // The explicit form also covers a bare directory name.
        match resolve_target("dir:skills").unwrap() {
            TargetSpec::Dir(dir) => assert_eq!(dir, PathBuf::from("skills")),
            _ => panic!("dir:skills must be a directory target"),
        }
        assert!(resolve_target("dir:").is_err());
    }

    #[test]
    fn install_then_check_is_up_to_date() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("skills").join(SKILL_DIR_NAME);
        assert_eq!(install_files(FILES, &dir).unwrap(), 2);
        let status = check_target(FILES, &dir);
        assert!(status.installed);
        assert!(status.up_to_date);
        assert!(status.missing.is_empty());
        assert!(status.differing.is_empty());
        // Parents were created, content written verbatim.
        assert_eq!(
            fs::read_to_string(dir.join("references/quickref.md")).unwrap(),
            "quickref\n"
        );
    }

    #[test]
    fn check_reports_missing_and_drift() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join(SKILL_DIR_NAME);
        let absent = check_target(FILES, &dir);
        assert!(!absent.installed);
        assert_eq!(absent.missing.len(), 2);

        install_files(FILES, &dir).unwrap();
        fs::write(dir.join("SKILL.md"), "# edited\n").unwrap();
        let drifted = check_target(FILES, &dir);
        assert!(drifted.installed);
        assert!(!drifted.up_to_date);
        assert_eq!(drifted.differing, vec!["SKILL.md"]);
    }

    #[test]
    fn skill_digest_is_stable_hex() {
        let digest = skill_digest(FILES).unwrap();
        assert_eq!(digest.len(), 64);
        let again = skill_digest(FILES).unwrap();
        assert_eq!(digest, again);
    }

    #[test]
    fn bundle_version_reads_the_generated_stamp() {
        const STAMPED: &[SkillFile] = &[SkillFile {
            rel: "SKILL.md",
            content: "# bundle\n- **Bundle version**: niubash 1.5.1 (generated; see scripts/generate-skill.py).\n",
        }];
        assert_eq!(bundle_version(STAMPED).as_deref(), Some("1.5.1"));
        // No stamp line, and no SKILL.md at all, both read as unknown.
        assert_eq!(bundle_version(FILES), None);
        assert_eq!(bundle_version(&[]), None);
    }

    /// niubash#1084: native and relative spellings are not dialect
    /// spellings — they pass through byte-for-byte on every platform,
    /// exactly as before the classifier routing.
    #[test]
    fn issue1084_native_and_relative_spellings_are_untouched() {
        for (spec, want) in [
            ("dir:F:/ext-build/probe", "F:/ext-build/probe"),
            ("dir:F:\\ext-build\\probe", "F:\\ext-build\\probe"),
            ("dir:skills", "skills"),
            ("./skills", "./skills"),
        ] {
            match resolve_target(spec).unwrap() {
                TargetSpec::Dir(dir) => assert_eq!(dir, PathBuf::from(want), "{spec}"),
                _ => panic!("{spec} must be a directory target"),
            }
        }
    }

    #[cfg(windows)]
    fn unique_temp_dir(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "niu-skill-{}-{}-{}",
            label,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[cfg(windows)]
    fn clear_root_env() {
        for name in ["NIU_ROOT", "RUBASH_ROOT", "__RUBASH_SHELL_ROOT"] {
            std::env::remove_var(name);
        }
    }

    /// niubash#1084: `/tmp/...` names the per-user temp namespace the
    /// shell resolves, anchored at TMPDIR — never the root of
    /// whichever drive the process cwd sits on. The resolved path is
    /// absolute, which is what makes install and status agree from
    /// any cwd on any drive.
    #[cfg(windows)]
    #[test]
    fn issue1084_tmp_spelling_resolves_to_the_temp_base() {
        let _env_lock = crate::test_support::PROCESS_STATE_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let _restore = crate::test_support::ProcessEnvRestore::new(&[
            "TMPDIR",
            "NIU_ROOT",
            "RUBASH_ROOT",
            "__RUBASH_SHELL_ROOT",
        ]);
        let tmp_base = unique_temp_dir("tmp-base");
        std::env::set_var("TMPDIR", &tmp_base);
        clear_root_env();

        for spec in ["dir:/tmp/issue1084-probe", "/tmp/issue1084-probe"] {
            match resolve_target(spec).unwrap() {
                TargetSpec::Dir(dir) => {
                    assert_eq!(dir, tmp_base.join("issue1084-probe"), "{spec}");
                    assert!(dir.is_absolute(), "{spec}");
                }
                _ => panic!("{spec} must be a directory target"),
            }
        }
        let _ = fs::remove_dir_all(tmp_base);
    }

    /// niubash#1084: the POSIX drive form names that drive from any
    /// cwd — `/f/...` issued on C: is `F:\...`, not `C:\f\...`.
    #[cfg(windows)]
    #[test]
    fn issue1084_drive_spelling_resolves_to_that_drive() {
        for (spec, want) in [
            ("dir:/f/ext-build/probe", "F:/ext-build/probe"),
            ("dir:/c", "C:/"),
        ] {
            match resolve_target(spec).unwrap() {
                TargetSpec::Dir(dir) => assert_eq!(dir, PathBuf::from(want), "{spec}"),
                _ => panic!("{spec} must be a directory target"),
            }
        }
    }

    /// niubash#1084: install-tree spellings resolve under the shell
    /// root the host published as NIU_ROOT (the snapshot mirrors it
    /// into the engine's lookup name), and `/home` names the users
    /// directory — the same answers `cd` gives.
    #[cfg(windows)]
    #[test]
    fn issue1084_root_spellings_resolve_under_the_shell_root() {
        let _env_lock = crate::test_support::PROCESS_STATE_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let _restore = crate::test_support::ProcessEnvRestore::new(&[
            "HOME",
            "NIU_ROOT",
            "RUBASH_ROOT",
            "__RUBASH_SHELL_ROOT",
        ]);
        let root = unique_temp_dir("shell-root");
        std::env::set_var("NIU_ROOT", &root);
        std::env::remove_var("RUBASH_ROOT");
        std::env::remove_var("__RUBASH_SHELL_ROOT");

        for (spec, want) in [
            ("dir:/etc/probe", root.join("etc").join("probe")),
            ("dir:/", root.clone()),
            ("/abs/skills", root.join("abs").join("skills")),
        ] {
            match resolve_target(spec).unwrap() {
                TargetSpec::Dir(dir) => assert_eq!(dir, want, "{spec}"),
                _ => panic!("{spec} must be a directory target"),
            }
        }

        let users = unique_temp_dir("users");
        let home = users.join("testuser");
        fs::create_dir_all(&home).unwrap();
        std::env::set_var("HOME", &home);
        match resolve_target("dir:/home/other").unwrap() {
            TargetSpec::Dir(dir) => assert_eq!(dir, users.join("other")),
            _ => panic!("dir:/home/other must be a directory target"),
        }
        let _ = fs::remove_dir_all(root);
        let _ = fs::remove_dir_all(users);
    }

    /// niubash#1084: a leading-`/` spelling the classifier has no
    /// ruling for is an error, never a silent landing on the current
    /// drive. `/dev/null` is the deterministic case: the classifier
    /// maps it to the NUL device in every environment, root or not,
    /// and a device is not a directory.
    #[cfg(windows)]
    #[test]
    fn issue1084_unclassifiable_spelling_is_rejected() {
        let _env_lock = crate::test_support::PROCESS_STATE_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let _restore = crate::test_support::ProcessEnvRestore::new(&[
            "NIU_ROOT",
            "RUBASH_ROOT",
            "__RUBASH_SHELL_ROOT",
        ]);
        clear_root_env();
        let err = match resolve_target("dir:/dev/null") {
            Ok(_) => panic!("dir:/dev/null must be rejected"),
            Err(e) => e.to_string(),
        };
        assert!(err.contains("/dev/null"), "{err}");
    }
}
