//! The declarative plugin spec — `~/.niubash/plugins.toml`
//! (design §14.6.3, owner ruling 2026-10-03: "添加插件应该用配置文件，
//! 不只是 CLI").
//!
//! The spec is the single source of truth for *what should be installed and
//! enabled*; the source registry (`plugins::sources`) stays the lockfile
//! for *what is installed* (commit + tree checksum pins) — the same
//! spec+lock pair lazy.nvim/vim-plug use (`lazy-lock.json`). `niu plugin
//! sync` (and the CLI verbs, which are sugar over spec edits + sync)
//! reconciles the two and materializes the managed rc blocks from the spec.
//!
//! ```toml
//! schema = "niubash:plugin-spec@0.1.0"
//!
//! [[sources]]
//! target = "oh-my-bash"            # catalog id | owner/repo | url | path
//! enable = ["git", "npm"]
//! theme  = "agnoster"
//!
//! [[sources]]
//! target = "rcaloras/bash-preexec" # wild file source (GitHub shorthand)
//! enable = ["bash-preexec.sh"]     # asset names are tree-relative paths
//! ```

use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::path_utils::shell_home_dir;

pub const PLUGIN_SPEC_SCHEMA: &str = "niubash:plugin-spec@0.1.0";

/// Where the spec lives: `$NIU_PLUGIN_SPEC` overrides (tests, portable
/// setups); default `~/.niubash/plugins.toml`.
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
        .join("plugins.toml")
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
    let spec: PluginSpec = toml::from_str(&text)?;
    Ok(Some(spec))
}

/// Write the spec (creating parent directories as needed).
pub fn save_spec(spec: &PluginSpec) -> anyhow::Result<()> {
    let path = spec_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut out = String::new();
    // A stable, hand-edit-friendly rendering: schema line, then entries in
    // declaration order with blank lines between them.
    if !spec.sources.is_empty() {
        out.push_str(&format!("schema = \"{PLUGIN_SPEC_SCHEMA}\"\n"));
    }
    for source in &spec.sources {
        out.push_str("\n[[sources]]\n");
        out.push_str(&format!("target = {}\n", toml_quote(&source.target)));
        if let Some(id) = &source.id {
            out.push_str(&format!("id = {}\n", toml_quote(id)));
        }
        if let Some(kind) = &source.kind {
            out.push_str(&format!("kind = {}\n", toml_quote(kind)));
        }
        if let Some(ref_name) = &source.ref_name {
            out.push_str(&format!("ref = {}\n", toml_quote(ref_name)));
        }
        if let Some(theme) = &source.theme {
            out.push_str(&format!("theme = {}\n", toml_quote(theme)));
        }
        if !source.enable.is_empty() {
            let items: Vec<String> = source.enable.iter().map(|n| toml_quote(n)).collect();
            out.push_str(&format!("enable = [{}]\n", items.join(", ")));
        }
    }
    if out.is_empty() {
        out.push_str(&format!(
            "# niubash plugin spec — declare sources here, then run `niu plugin sync`.\n\
             # schema = \"{PLUGIN_SPEC_SCHEMA}\"\n"
        ));
    }
    fs::write(&path, out)?;
    Ok(())
}

fn toml_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', r"'\''"))
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
        let path = dir.join("plugins.toml");
        let guard = EnvGuard::set(&path);
        (path, guard)
    }

    #[test]
    fn spec_round_trips_through_toml() {
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
        assert!(text.contains("[[sources]]"), "{text}");
        assert!(text.contains("target = 'oh-my-bash'"), "{text}");
        assert!(text.contains("theme = 'agnoster'"), "{text}");
        assert!(text.contains("enable = ['git', 'npm']"), "{text}");
        assert!(text.contains("target = 'rcaloras/bash-preexec'"), "{text}");
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
        assert!(text.contains("niu plugin sync"), "{text}");
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
}
