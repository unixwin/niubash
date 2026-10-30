//! The declarative plugin spec — `~/.niubash/plugins.json`
//! (design §14.6.3, owner ruling 2026-10-03: "添加插件应该用配置文件，
//! 不只是 CLI").
//!
//! The spec is the single source of truth for *what should be installed and
//! enabled*; the source registry (`plugins::sources`) stays the lockfile
//! for *what is installed* (commit pins) — the same
//! spec+lock pair lazy.nvim/vim-plug use (`lazy-lock.json`). `niu plugin
//! sync` (and the CLI verbs, which are sugar over spec edits + sync)
//! reconciles the two and materializes the managed rc blocks from the spec.
//!
//! ```json
//! {
//!   "schema": "niubash:plugin-spec@0.1.0",
//!   "sources": [
//!     { "target": "oh-my-bash", "enable": ["git", "npm"], "theme": "agnoster" },
//!     { "target": "rcaloras/bash-preexec", "enable": ["bash-preexec.sh"] }
//!   ]
//! }
//! ```
//!
//! The spec is JSON since the 1.5.0 product-TOML deletion; the pre-1.5.0
//! TOML spec (`plugins.toml`, same directory) is converted exactly once
//! by [`migrate_legacy_spec`] (`niu plugin migrate`) — no runtime path
//! reads it.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Context;
use serde::{Deserialize, Serialize};

use crate::path_utils::shell_home_dir;

pub const PLUGIN_SPEC_SCHEMA: &str = "niubash:plugin-spec@0.1.0";

/// Pre-1.5.0 TOML spec name. Read exactly once, by
/// [`migrate_legacy_spec`]; no runtime path looks for it.
const LEGACY_SPEC_FILE: &str = "plugins.toml";

/// Where the spec lives: `$NIU_PLUGIN_SPEC` overrides (tests, portable
/// setups); default `~/.niubash/plugins.json`.
pub fn spec_path() -> PathBuf {
    if let Some(value) = std::env::var_os("NIU_PLUGIN_SPEC") {
        let path = PathBuf::from(value);
        if !path.as_os_str().is_empty() {
            return path;
        }
    }
    shell_home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".niubash")
        .join("plugins.json")
}

/// Where the pre-1.5.0 TOML spec lived: the sibling of [`spec_path`]
/// under its retired name (migrate-only).
fn legacy_spec_path() -> PathBuf {
    spec_path().with_file_name(LEGACY_SPEC_FILE)
}

/// The parsed spec file.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PluginSpec {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sources: Vec<SpecSource>,
}

/// One declared source.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SpecSource {
    /// Install target as the user wrote it: catalog id (`oh-my-bash`),
    /// GitHub shorthand (`owner/repo`), git URL, or local path. Resolution
    /// happens at sync time, exactly like `niu plugin add`.
    pub target: String,
    /// Explicit source id (wild/bpkg sources otherwise derive one from the
    /// origin tail at sync time).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// Explicit adapter kind pin (`"bpkg"`): when the user adopted a tree
    /// as a specific manager (`niu plugin add bpkg --path <dir>`), later
    /// syncs must re-install through that adapter — and refuse when the
    /// tree stops matching its fingerprint — instead of silently falling
    /// back to wild-file detection. Catalog-id targets do not need this
    /// (the id resolves the adapter).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// Git ref to install (first fetch only; later syncs never move the
    /// lockfile pin — `niu plugin update` does).
    #[serde(rename = "ref", default, skip_serializing_if = "Option::is_none")]
    pub ref_name: Option<String>,
    /// Theme pick for managers with a theme variable (`OSH_THEME`,
    /// `BASH_IT_THEME`). Absent = "don't manage the theme" (a hand-set
    /// value survives syncs).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub theme: Option<String>,
    /// Enabled asset names (manager-native meaning: OMB rc arrays, bash-it
    /// enabled/ entries, per-file source lines for wild/bpkg sources).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub enable: Vec<String>,
}

/// Read the spec. `Ok(None)` = no spec file yet (imperative/legacy mode:
/// nothing is declared, sync only reports).
pub fn load_spec() -> anyhow::Result<Option<PluginSpec>> {
    let path = spec_path();
    if !path.is_file() {
        return Ok(None);
    }
    let text = fs::read_to_string(&path)?;
    let spec: PluginSpec = serde_json::from_str(&text)?;
    Ok(Some(spec))
}

/// Render the spec document: pretty JSON with the schema marker stamped
/// whenever sources are declared (so a hand-editor sees the format name
/// first, the role the hand-rendered TOML header line used to play); an
/// empty spec renders as the minimal `{}`.
fn render_spec(spec: &PluginSpec) -> anyhow::Result<String> {
    let mut rendered = spec.clone();
    if !rendered.sources.is_empty() && rendered.schema.is_none() {
        rendered.schema = Some(PLUGIN_SPEC_SCHEMA.to_string());
    }
    let mut body = serde_json::to_string_pretty(&rendered)?;
    body.push('\n');
    Ok(body)
}

fn write_spec_to(path: &Path, spec: &PluginSpec) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    // niubash#359 (design §4.4): the spec is control-plane state, so it
    // gets the same #304 atomic swap (a concurrent reader — a sync or the
    // doctor in another terminal — never observes a truncate window). The
    // bare fs::write here was the exact hazard the rc writers had already
    // retired.
    crate::path_utils::write_file_atomic(path, render_spec(spec)?.as_bytes())?;
    Ok(())
}

/// Write the spec (creating parent directories as needed).
pub fn save_spec(spec: &PluginSpec) -> anyhow::Result<()> {
    write_spec_to(&spec_path(), spec)
}

/// Outcome of [`migrate_legacy_spec`]: what the one-shot bridge moved.
#[derive(Debug, Clone)]
pub struct SpecMigrationReport {
    /// The legacy spec file, renamed aside (present iff one was converted).
    pub backup: Option<PathBuf>,
    /// Targets the legacy spec declared that the JSON spec did not
    /// already carry (merged in by target).
    pub sources: Vec<String>,
    /// The legacy spec, left in place because it could not be converted
    /// (named, with the reason — never guessed at).
    pub skipped: Vec<String>,
}

/// One-time migration from the pre-1.5.0 TOML spec (`plugins.toml`) to
/// the JSON spec (`plugins.json`). This is the ONLY code that still
/// reads the TOML form — the runtime never does (the same one-shot
/// pattern as `niu config migrate` on the config face and
/// `distros::migrate_legacy_distros` on the distro face). The legacy
/// file is renamed to `<name>.migrated-<UTC timestamp>`, never deleted.
/// When there is no legacy spec this returns `Ok(None)`; a spec that
/// does not parse as the legacy shape is skipped and named in the
/// report (left in place, fixable by hand, re-runnable) while the rest
/// of `niu plugin migrate` still proceeds. An existing JSON spec wins
/// entry-by-entry (keyed by `target`); legacy entries it does not
/// declare are appended. A JSON spec niu cannot read refuses the
/// conversion before anything on disk changes.
pub fn migrate_legacy_spec() -> anyhow::Result<Option<SpecMigrationReport>> {
    let legacy_path = legacy_spec_path();
    let json_path = if legacy_path == spec_path() {
        // An override still naming the retired file itself: the JSON
        // form takes the same stem with the new extension.
        legacy_path.with_extension("json")
    } else {
        spec_path()
    };
    let text = match fs::read_to_string(&legacy_path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => {
            return Err(err).with_context(|| format!("could not read {}", legacy_path.display()))
        }
    };
    let legacy_spec: PluginSpec = match toml::from_str(&text) {
        Ok(spec) => spec,
        Err(err) => {
            return Ok(Some(SpecMigrationReport {
                backup: None,
                sources: Vec::new(),
                skipped: vec![format!(
                    "{}: could not parse the legacy TOML spec ({err})",
                    legacy_path.display()
                )],
            }));
        }
    };
    let mut merged = PluginSpec::default();
    if json_path.is_file() {
        let existing_text = fs::read_to_string(&json_path)?;
        merged = serde_json::from_str(&existing_text).with_context(|| {
            format!(
                "could not parse {} — fix or remove the file by hand; \
                 niu never rewrites a spec it cannot read",
                json_path.display()
            )
        })?;
    }
    if merged.schema.is_none() {
        merged.schema = legacy_spec.schema.clone();
    }
    let mut carried = Vec::new();
    for entry in legacy_spec.sources {
        if !merged.sources.iter().any(|e| e.target == entry.target) {
            carried.push(entry.target.clone());
            merged.sources.push(entry);
        }
    }
    write_spec_to(&json_path, &merged)?;
    let backup = super::distros::rename_legacy_file(&legacy_path)?;
    Ok(Some(SpecMigrationReport {
        backup: Some(backup),
        sources: carried,
        skipped: Vec::new(),
    }))
}

impl PluginSpec {
    /// Find the entry declaring the given source id (explicit `id`, or the
    /// id a catalog target resolves to).
    pub fn entry_for_id(&self, id: &str) -> Option<&SpecSource> {
        self.sources
            .iter()
            .find(|source| resolve_entry_id(source).as_deref() == Some(id))
    }

    /// Entry index by id (for mutation).
    pub fn entry_index_for_id(&self, id: &str) -> Option<usize> {
        self.sources
            .iter()
            .position(|source| resolve_entry_id(source).as_deref() == Some(id))
    }
}

/// The id a spec entry resolves to without fetching: an explicit `id`, a
/// catalog id target (`oh-my-bash`), or `None` (derived from the origin
/// tail at sync time — wild file sources).
pub fn resolve_entry_id(entry: &SpecSource) -> Option<String> {
    if let Some(id) = &entry.id {
        return Some(id.clone());
    }
    if !entry.target.contains('/')
        && !entry.target.contains('\\')
        && !entry.target.contains("://")
        && !PathBuf::from(&entry.target).is_absolute()
        && !entry.target.starts_with("./")
        && !entry.target.starts_with("../")
    {
        // A bare word may be a catalog id; manager ids are the only
        // resolvable-without-fetch ids.
        if let Some(adapter) = super::sources::adapter_for(&entry.target) {
            if adapter.id() != "file" {
                return Some(adapter.id().to_string());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    struct EnvGuard {
        previous: Option<std::ffi::OsString>,
    }

    impl EnvGuard {
        fn set(value: &PathBuf) -> Self {
            let previous = std::env::var_os("NIU_PLUGIN_SPEC");
            std::env::set_var("NIU_PLUGIN_SPEC", value);
            Self { previous }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match &self.previous {
                Some(value) => std::env::set_var("NIU_PLUGIN_SPEC", value),
                None => std::env::remove_var("NIU_PLUGIN_SPEC"),
            }
        }
    }

    fn temp_spec(label: &str) -> (PathBuf, EnvGuard) {
        let dir = std::env::temp_dir().join(format!(
            "niu-spec-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("plugins.json");
        let guard = EnvGuard::set(&path);
        (path, guard)
    }

    #[test]
    fn spec_round_trips_through_json() {
        let _env_lock = crate::test_support::PROCESS_STATE_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let (path, _guard) = temp_spec("round-trip");
        let spec = PluginSpec {
            schema: Some(PLUGIN_SPEC_SCHEMA.to_string()),
            sources: vec![
                SpecSource {
                    target: "oh-my-bash".to_string(),
                    id: None,
                    kind: None,
                    ref_name: None,
                    theme: Some("agnoster".to_string()),
                    enable: vec!["git".to_string(), "npm".to_string()],
                },
                SpecSource {
                    target: "rcaloras/bash-preexec".to_string(),
                    id: Some("bash-preexec".to_string()),
                    kind: None,
                    ref_name: None,
                    theme: None,
                    enable: vec!["bash-preexec.sh".to_string()],
                },
            ],
        };
        save_spec(&spec).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains("\"sources\""), "{text}");
        assert!(text.contains("\"target\": \"oh-my-bash\""), "{text}");
        assert!(text.contains("\"theme\": \"agnoster\""), "{text}");
        assert!(text.contains("\"enable\""), "{text}");
        assert!(text.contains("\"git\""), "{text}");
        assert!(
            text.contains("\"target\": \"rcaloras/bash-preexec\""),
            "{text}"
        );
        // Reload parses back to the same spec.
        let loaded = load_spec().unwrap().expect("spec present");
        assert_eq!(loaded, spec);
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn missing_spec_reads_as_none_and_empty_spec_writes_a_starter() {
        let _env_lock = crate::test_support::PROCESS_STATE_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let (path, _guard) = temp_spec("missing");
        assert!(load_spec().unwrap().is_none());
        save_spec(&PluginSpec::default()).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert_eq!(text.trim(), "{}", "{text}");
        // An empty spec still parses as present-but-empty.
        assert_eq!(load_spec().unwrap().unwrap().sources, Vec::new());
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn entry_ids_resolve_without_fetching() {
        let spec = PluginSpec {
            schema: None,
            sources: vec![
                SpecSource {
                    target: "oh-my-bash".to_string(),
                    id: None,
                    kind: None,
                    ref_name: None,
                    theme: None,
                    enable: vec![],
                },
                SpecSource {
                    target: "https://github.com/rcaloras/bash-preexec.git".to_string(),
                    id: Some("preexec".to_string()),
                    kind: None,
                    ref_name: None,
                    theme: None,
                    enable: vec![],
                },
            ],
        };
        assert_eq!(
            spec.entry_for_id("oh-my-bash").unwrap().target,
            "oh-my-bash"
        );
        assert_eq!(
            spec.entry_for_id("preexec").unwrap().target,
            "https://github.com/rcaloras/bash-preexec.git"
        );
        assert!(spec.entry_for_id("unknown").is_none());
        // A bare non-catalog word resolves to nothing here (derived at
        // sync time from the origin).
        let wild = SpecSource {
            target: "owner/repo".to_string(),
            id: None,
            kind: None,
            ref_name: None,
            theme: None,
            enable: vec![],
        };
        assert_eq!(resolve_entry_id(&wild), None);
    }

    const LEGACY_SPEC_TOML: &str = "schema = \"niubash:plugin-spec@0.1.0\"\n\
        \n\
        [[sources]]\n\
        target = \"oh-my-bash\"\n\
        enable = [\"git\", \"npm\"]\n\
        theme = \"agnoster\"\n\
        \n\
        [[sources]]\n\
        target = \"rcaloras/bash-preexec\"\n\
        enable = [\"bash-preexec.sh\"]\n";

    #[test]
    fn migrate_converts_the_legacy_toml_spec() {
        let _env_lock = crate::test_support::PROCESS_STATE_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let (path, _guard) = temp_spec("migrate");
        let dir = path.parent().unwrap().to_path_buf();
        let legacy_path = dir.join("plugins.toml");
        fs::write(&legacy_path, LEGACY_SPEC_TOML).unwrap();

        // The runtime is blind to the TOML form until migrate runs.
        assert!(load_spec().unwrap().is_none());

        let report = migrate_legacy_spec()
            .unwrap()
            .expect("legacy spec migrates");
        assert_eq!(
            report.sources,
            ["oh-my-bash", "rcaloras/bash-preexec"],
            "{report:?}"
        );
        let backup = report.backup.expect("legacy renamed aside");
        assert!(
            backup
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("plugins.toml.migrated-"),
            "{backup:?}"
        );
        assert!(!legacy_path.exists(), "legacy renamed, not copied");
        assert_eq!(
            fs::read_to_string(&backup).unwrap(),
            LEGACY_SPEC_TOML,
            "the backup keeps the original bytes"
        );

        let loaded = load_spec().unwrap().expect("spec present after migrate");
        assert_eq!(loaded.sources.len(), 2);
        assert_eq!(loaded.sources[0].theme.as_deref(), Some("agnoster"));
        assert_eq!(loaded.sources[0].enable, ["git", "npm"]);
        assert_eq!(loaded.sources[1].target, "rcaloras/bash-preexec");

        // Second run: nothing left to migrate.
        assert!(migrate_legacy_spec().unwrap().is_none());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn migrate_merges_into_an_existing_json_spec_by_target() {
        let _env_lock = crate::test_support::PROCESS_STATE_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let (path, _guard) = temp_spec("migrate-merge");
        let dir = path.parent().unwrap().to_path_buf();
        // The JSON spec already declares oh-my-bash (its theme wins);
        // the legacy spec re-declares it and adds one more source.
        save_spec(&PluginSpec {
            schema: None,
            sources: vec![SpecSource {
                target: "oh-my-bash".to_string(),
                id: None,
                kind: None,
                ref_name: None,
                theme: Some("demox".to_string()),
                enable: vec!["git".to_string()],
            }],
        })
        .unwrap();
        fs::write(dir.join("plugins.toml"), LEGACY_SPEC_TOML).unwrap();

        let report = migrate_legacy_spec()
            .unwrap()
            .expect("legacy spec migrates");
        assert_eq!(report.sources, ["rcaloras/bash-preexec"], "{report:?}");
        let loaded = load_spec().unwrap().expect("spec present");
        assert_eq!(loaded.sources.len(), 2);
        assert_eq!(
            loaded.sources[0].theme.as_deref(),
            Some("demox"),
            "the JSON entry wins by target"
        );
        assert_eq!(loaded.sources[1].target, "rcaloras/bash-preexec");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn migrate_skips_an_unconvertible_spec_and_leaves_it() {
        let _env_lock = crate::test_support::PROCESS_STATE_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let (path, _guard) = temp_spec("migrate-bad");
        let dir = path.parent().unwrap().to_path_buf();
        let legacy_path = dir.join("plugins.toml");
        fs::write(&legacy_path, "[invalid TOML\n").unwrap();

        let report = migrate_legacy_spec()
            .unwrap()
            .expect("a bad spec is reported, not an error");
        assert!(report.backup.is_none(), "{report:?}");
        assert_eq!(report.skipped.len(), 1, "{report:?}");
        assert!(
            report.skipped[0].contains("plugins.toml"),
            "{}",
            report.skipped[0]
        );
        assert!(legacy_path.is_file(), "the bad spec is left in place");
        assert!(!path.exists(), "no JSON spec was written");
        let _ = fs::remove_dir_all(&dir);
    }
}
