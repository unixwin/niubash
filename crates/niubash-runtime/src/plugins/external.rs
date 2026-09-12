//! Third-party bundle install channel.
//!
//! External bundles live under `~/.niubash/external/<name>` and are
//! registered in `~/.niubash/external/registry.toml`. New bundles are
//! **untrusted** until an explicit `niu plugin trust <name>`; the inventory
//! resolver skips untrusted external bundles so none of their packs activate.
//! Activation goes through the same plugin-lock channel as the official
//! bundle (`niu plugin use <name>`), which keeps `niu plugin rollback`
//! working for external bundles too.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::Context;
use serde::{Deserialize, Serialize};

use crate::path_utils::shell_home_dir;

/// One registered external bundle.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExternalBundleRecord {
    pub name: String,
    pub url: String,
    #[serde(rename = "ref")]
    pub ref_name: String,
    pub path: PathBuf,
    pub trusted: bool,
    pub added_at: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct ExternalRegistryToml {
    #[serde(default)]
    bundles: Vec<ExternalBundleRecord>,
}

/// Root directory for third-party bundles: `~/.niubash/external`.
/// `NIU_EXTERNAL_BUNDLE_ROOT` overrides the location (tests and portable
/// setups).
pub fn external_root() -> PathBuf {
    if let Some(value) = std::env::var_os("NIU_EXTERNAL_BUNDLE_ROOT") {
        let path = PathBuf::from(value);
        if !path.as_os_str().is_empty() {
            return path;
        }
    }
    shell_home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".niubash")
        .join("external")
}

fn registry_path() -> PathBuf {
    external_root().join("registry.toml")
}

/// Read all registered external bundles.
pub fn read_registry() -> Vec<ExternalBundleRecord> {
    let Ok(text) = fs::read_to_string(registry_path()) else {
        return Vec::new();
    };
    toml::from_str::<ExternalRegistryToml>(&text)
        .map(|registry| registry.bundles)
        .unwrap_or_else(|err| {
            log::warn!("failed to parse external bundle registry: {}", err);
            Vec::new()
        })
}

fn write_registry(bundles: &[ExternalBundleRecord]) -> anyhow::Result<()> {
    let path = registry_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let text = toml::to_string_pretty(&ExternalRegistryToml {
        bundles: bundles.to_vec(),
    })?;
    fs::write(path, text)?;
    Ok(())
}

/// True when the path is a registered external bundle directory.
pub fn is_external_bundle_path(path: &Path) -> bool {
    path.starts_with(external_root()) && path != external_root()
}

/// Trust state for a path that lives under the external root. `None` when the
/// path is not a registered external bundle.
pub fn external_bundle_trusted(path: &Path) -> Option<bool> {
    if !is_external_bundle_path(path) {
        return None;
    }
    read_registry()
        .into_iter()
        .find(|record| record.path == path)
        .map(|record| record.trusted)
}

fn derive_name_from_url(url: &str) -> String {
    let trimmed = url.trim_end_matches('/');
    let last = trimmed.rsplit(['/', '\\']).next().unwrap_or(trimmed);
    last.strip_suffix(".git").unwrap_or(last).to_string()
}

fn now_timestamp() -> String {
    // Local wall-clock timestamp; failure falls back to unix seconds.
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs().to_string())
        .unwrap_or_default()
}

/// Clone a git repository as a third-party bundle and register it untrusted.
pub fn add_bundle(url: &str, name_override: Option<&str>) -> anyhow::Result<ExternalBundleRecord> {
    let url = url.trim();
    if url.is_empty() {
        anyhow::bail!("bundle url is empty");
    }
    let (repo_url, ref_name) = match url.split_once('@') {
        // Only treat the tail as a ref when the URL itself has no '@'
        // (git scp-like syntax user@host is not supported by this channel).
        Some((repo, ref_name)) if !repo.contains('@') && !ref_name.is_empty() => {
            (repo, ref_name.to_string())
        }
        _ => (url, "HEAD".to_string()),
    };

    let derived = derive_name_from_url(repo_url);
    let name = name_override.map(str::trim).filter(|n| !n.is_empty());
    let name = match name {
        Some(name) => name.to_string(),
        None => derived,
    };
    if !safe_bundle_name(&name) {
        anyhow::bail!(
            "invalid bundle name '{}': use letters, digits, '-' and '_' only",
            name
        );
    }

    let mut registry = read_registry();
    if registry.iter().any(|record| record.name == name) {
        anyhow::bail!(
            "external bundle '{}' already exists; remove it first with niu plugin remove {}",
            name,
            name
        );
    }

    let path = external_root().join(&name);
    if path.exists() {
        anyhow::bail!("directory {} already exists", path.display());
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    let mut command = Command::new("git");
    command.arg("clone").arg("--depth").arg("1").arg(repo_url);
    if ref_name != "HEAD" {
        command.arg("--branch").arg(&ref_name);
    }
    command.arg(&path);
    let status = command
        .status()
        .with_context(|| "failed to run git; is git.exe on PATH?")?;
    if !status.success() {
        let _ = fs::remove_dir_all(&path);
        anyhow::bail!(
            "git clone exited with status {}",
            status.code().unwrap_or(1)
        );
    }

    let bundle_toml = path.join("bundle.toml");
    if !bundle_toml.is_file() {
        let _ = fs::remove_dir_all(&path);
        anyhow::bail!(
            "{} does not contain a bundle.toml; not a niubash bundle",
            name
        );
    }

    let record = ExternalBundleRecord {
        name: name.clone(),
        url: repo_url.to_string(),
        ref_name,
        path: path.clone(),
        trusted: false,
        added_at: now_timestamp(),
    };
    registry.push(record.clone());
    write_registry(&registry)?;

    Ok(record)
}

/// Mark a registered external bundle as trusted.
pub fn trust_bundle(name: &str) -> anyhow::Result<ExternalBundleRecord> {
    let mut registry = read_registry();
    let Some(record) = registry.iter_mut().find(|record| record.name == name) else {
        anyhow::bail!(
            "unknown external bundle '{}'; run niu plugin add first",
            name
        );
    };
    record.trusted = true;
    let trusted = record.clone();
    write_registry(&registry)?;
    Ok(trusted)
}

/// Remove a registered external bundle: delete its directory and the
/// registry entry. Trust state does not matter for removal.
pub fn remove_bundle(name: &str) -> anyhow::Result<PathBuf> {
    let mut registry = read_registry();
    let Some(index) = registry.iter().position(|record| record.name == name) else {
        anyhow::bail!("unknown external bundle '{}'", name);
    };
    let record = registry.remove(index);
    write_registry(&registry)?;

    // Only delete directories we registered; never follow a tampered record.
    let expected_root = external_root();
    if record.path.starts_with(&expected_root)
        && record.path != expected_root
        && record.path.join("bundle.toml").is_file()
    {
        fs::remove_dir_all(&record.path)
            .with_context(|| format!("failed to remove {}", record.path.display()))?;
    }
    Ok(record.path)
}

fn safe_bundle_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::PROCESS_STATE_LOCK;

    #[test]
    fn derive_name_strips_git_suffix() {
        assert_eq!(
            derive_name_from_url("https://example.com/foo/bar.git"),
            "bar"
        );
        assert_eq!(derive_name_from_url("https://example.com/foo/bar/"), "bar");
    }

    #[test]
    fn add_rejects_bad_names() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap();
        let temp = std::env::temp_dir().join(format!(
            "niu-external-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::env::set_var("NIU_EXTERNAL_BUNDLE_ROOT", &temp);
        let err = add_bundle("https://example.com/foo/bar.git", Some("bad name!"))
            .expect_err("bad name must fail");
        assert!(err.to_string().contains("invalid bundle name"));
        std::env::remove_var("NIU_EXTERNAL_BUNDLE_ROOT");
        let _ = std::fs::remove_dir_all(&temp);
    }

    #[test]
    fn external_root_honors_override() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap();
        let temp = std::env::temp_dir().join(format!(
            "niu-external-root-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::env::set_var("NIU_EXTERNAL_BUNDLE_ROOT", &temp);
        assert_eq!(external_root(), temp);
        assert!(is_external_bundle_path(&temp.join("some-bundle")));
        assert!(!is_external_bundle_path(&temp));
        std::env::remove_var("NIU_EXTERNAL_BUNDLE_ROOT");
        let _ = std::fs::remove_dir_all(&temp);
    }

    #[test]
    fn add_trust_remove_round_trip_with_local_git_remote() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let base =
            std::env::temp_dir().join(format!("niu-external-e2e-{}-{}", std::process::id(), nanos));
        let remote = base.join("remote-repo");
        let root = base.join("external");
        std::fs::create_dir_all(&remote).unwrap();
        std::fs::write(
            remote.join("bundle.toml"),
            "name = \"demo-bundle\"\nversion = \"0.1.0\"\napi = \"1\"\nmin_niubash = \"1.0\"\n",
        )
        .unwrap();
        let git = |args: &[&str]| {
            let status = Command::new("git")
                .args(["-c", "user.email=t@example.com", "-c", "user.name=t"])
                .arg("-C")
                .arg(&remote)
                .args(args)
                .status()
                .expect("git must be available");
            assert!(status.success(), "git {args:?} failed");
        };
        git(&["init", "-q"]);
        git(&["add", "."]);
        git(&["commit", "-q", "-m", "init"]);

        std::env::set_var("NIU_EXTERNAL_BUNDLE_ROOT", &root);
        let record = add_bundle(remote.to_str().unwrap(), None).expect("add must succeed");
        assert_eq!(record.name, "remote-repo");
        assert!(!record.trusted);
        assert_eq!(external_bundle_trusted(&record.path), Some(false));

        let trusted = trust_bundle("remote-repo").expect("trust must succeed");
        assert!(trusted.trusted);
        assert_eq!(external_bundle_trusted(&record.path), Some(true));

        let removed = remove_bundle("remote-repo").expect("remove must succeed");
        assert!(!removed.exists());
        assert_eq!(external_bundle_trusted(&record.path), None);

        std::env::remove_var("NIU_EXTERNAL_BUNDLE_ROOT");
        let _ = std::fs::remove_dir_all(&base);
    }
}
