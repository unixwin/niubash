//! Git-transport mirroring — the China-network comfort layer (design
//! §14.8; download retraction 2026-10-04: git-only). The former HTTPS
//! prefix-rewrite channel (ghproxy-class download proxies) and the
//! reachability probe retired with the download driver: niu carries zero
//! network/HTTP responsibility, so the only traffic left to mirror is
//! `git clone` / `git fetch`, rewritten through git's own `insteadOf`
//! mechanism at transport time.
//!
//! Model: one user-editable config file `~/.niubash/mirrors.toml` holding
//! at most a `custom` mirror (a git insteadOf base) or `active = "none"`
//! for direct connection. `niu plugin mirror set <url>` writes it in one
//! command; community mirror examples live as comments, not code — they
//! may die at any moment and are the user's to verify.
//!
//! Iron invariant (§14.8): **rewrites happen at the transport layer only.**
//! The spec (`plugins.toml`), the lock (`registry.toml`) and recipe URLs
//! keep canonical GitHub origins, so a tree installed through a mirror
//! stays identical to a direct install and the lockfile remains portable
//! across machines and networks. Mirroring is also never a trust signal:
//! checksums and the trust protocol are unaffected. No auto-select, no
//! probing: the user pastes the URL they trust.
//!
//! Degrade discipline: a missing file, unknown `active` name, or malformed
//! TOML degrades to direct connection in the transport (a mirror config
//! problem must never break a fetch); `niu plugin mirror list` and `niu
//! doctor` surface the condition instead. Legacy `[github] prefix` /
//! `[github.releases]` download channels in an existing mirrors.toml
//! parse compatibly and are ignored — niu no longer has a download
//! transport for them to affect.

use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::path_utils::shell_home_dir;

pub const MIRRORS_SCHEMA: &str = "niubash:mirrors@0.1.0";

/// The canonical GitHub HTTPS origin eligible for rewriting. Everything
/// else (company GitLab, local paths, other hosts) passes through — a
/// mirror base must never silently capture traffic it does not proxy.
const GITHUB_HOST: &str = "https://github.com/";

/// Where mirrors.toml lives: `$NIU_MIRRORS` overrides (tests, portable
/// setups); default `~/.niubash/mirrors.toml` (the spec.rs convention).
pub fn mirrors_path() -> PathBuf {
    if let Some(value) = std::env::var_os("NIU_MIRRORS") {
        let path = PathBuf::from(value);
        if !path.as_os_str().is_empty() {
            return path;
        }
    }
    shell_home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".niubash")
        .join("mirrors.toml")
}

/// The parsed mirrors.toml. `Ok(None)` = no file yet (direct connection).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct MirrorConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema: Option<String>,
    /// `"none"` (direct, the default) or `"custom"` (use the `[github]`
    /// section). Any other value degrades to direct in the transport and is
    /// surfaced by `mirror list` / `niu doctor`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active: Option<String>,
    /// The custom mirror definition, consulted only when
    /// `active = "custom"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub github: Option<GithubMirror>,
}

/// The user-editable custom mirror (`[github]` in mirrors.toml).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct GithubMirror {
    /// git insteadOf base (gitclone class):
    /// `git -c url.<base>.insteadOf=https://github.com/`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_instead_of: Option<String>,
}

pub fn load_mirror_config() -> anyhow::Result<Option<MirrorConfig>> {
    let path = mirrors_path();
    if !path.is_file() {
        return Ok(None);
    }
    let text = fs::read_to_string(&path)?;
    let config: MirrorConfig = toml::from_str(&text)?;
    Ok(Some(config))
}

/// Render the config as the file users edit, with the example services as
/// comments (the "community mirror, may die anytime" caveat ships in the
/// file itself, not as code).
fn render_config(config: &MirrorConfig) -> String {
    let mut text = String::new();
    text.push_str(&format!("schema = \"{MIRRORS_SCHEMA}\"\n"));
    let active = config.active.as_deref().unwrap_or("none");
    text.push_str(&format!("active = \"{active}\"\n"));
    let github = config.github.clone().unwrap_or_default();
    if let Some(git) = github.git_instead_of.as_deref().filter(|g| !g.is_empty()) {
        text.push_str("\n[github]\n");
        text.push_str(&format!("git_instead_of = \"{git}\"\n"));
    }
    text.push_str(
        "\n\
        # ── notes ──────────────────────────────────────────────────────────\n\
        # `niu plugin mirror set <url>` rewrites this file for you;\n\
        # `niu plugin mirror set none` goes back to direct connection.\n\
        # git_instead_of = git-only mirror (git clone/fetch rewrites\n\
        #               https://github.com/ to this base). niu has no other\n\
        #               network transport (download retraction 2026-10-04).\n\
        #\n\
        # Community mirror examples — UNSUPPORTED and may disappear at any\n\
        # time; verify a service works for you before relying on it:\n\
        #   git_instead_of = \"https://gitclone.com/github.com\"\n\
        #   git_instead_of = \"https://ghproxy.net/github.com\"\n\
        # (niubash#303: the second example replaces a mirror that died\n\
        # years ago; both suggestions were verified live, git transport\n\
        # included, at edit time.)\n",
    );
    text
}

fn write_mirror_config(config: &MirrorConfig) -> anyhow::Result<()> {
    let path = mirrors_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&path, render_config(config))?;
    Ok(())
}

/// Normalize a pasted mirror URL into an insteadOf base: keep a trailing
/// `/` so `git` replaces the full `https://github.com/` prefix cleanly
/// (the "paste it and it works" bar, §14.8).
fn normalize_git_base(url: &str) -> anyhow::Result<String> {
    let url = url.trim().trim_end_matches('/');
    if !url.starts_with("https://") && !url.starts_with("http://") {
        anyhow::bail!(
            "mirror url must start with https:// (or http:// for a local proxy): '{url}'"
        );
    }
    if url == "https://" || url == "http://" {
        anyhow::bail!("mirror url has no host");
    }
    Ok(format!("{url}/"))
}

/// Switch to direct connection (`niu plugin mirror set none`). Preserves
/// the `[github]` section so re-enabling is a one-line `active` flip.
pub fn set_direct() -> anyhow::Result<()> {
    let mut config = load_mirror_config()?.unwrap_or_default();
    config.schema = Some(MIRRORS_SCHEMA.to_string());
    config.active = Some("none".to_string());
    write_mirror_config(&config)
}

/// Configure a git-only mirror in one command (`niu plugin mirror set
/// <url>`): validates the URL, normalizes the trailing slash, sets
/// `active = "custom"` and `[github] git_instead_of`. The value is
/// returned for the confirmation message.
pub fn set_custom_git_mirror(url: &str) -> anyhow::Result<String> {
    let base = normalize_git_base(url)?;
    let mut config = load_mirror_config()?.unwrap_or_default();
    config.schema = Some(MIRRORS_SCHEMA.to_string());
    config.active = Some("custom".to_string());
    let github = config.github.get_or_insert_with(Default::default);
    github.git_instead_of = Some(base.clone());
    write_mirror_config(&config)?;
    Ok(base)
}

/// The mirror the git transport should use right now. Infallible by
/// design: missing file, malformed TOML, and unknown names all degrade to
/// the `none` mirror (list/doctor report the condition; fetches never
/// break).
pub fn resolve_active_mirror() -> ResolvedMirror {
    let Ok(Some(config)) = load_mirror_config() else {
        return ResolvedMirror::none();
    };
    match config.active.as_deref() {
        Some("custom") => {
            let github = config.github.unwrap_or_default();
            ResolvedMirror {
                name: "custom".to_string(),
                git_instead_of_base: github.git_instead_of,
            }
        }
        _ => ResolvedMirror::none(),
    }
}

/// The fully-resolved mirror the git transport consults.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ResolvedMirror {
    pub name: String,
    pub git_instead_of_base: Option<String>,
}

impl ResolvedMirror {
    pub fn none() -> Self {
        Self {
            name: "none".to_string(),
            git_instead_of_base: None,
        }
    }

    pub fn is_none(&self) -> bool {
        self.name == "none"
    }
}

/// Extra git arguments implementing insteadOf rewriting for `origin`
/// (pure; returns complete `-c` argument pairs to splice before the git
/// subcommand). Non-GitHub origins get nothing.
pub fn git_clone_args_with(mirror: &ResolvedMirror, origin: &str) -> Vec<String> {
    let Some(base) = mirror
        .git_instead_of_base
        .as_deref()
        .filter(|base| !base.is_empty())
    else {
        return Vec::new();
    };
    if !origin.starts_with(GITHUB_HOST) {
        return Vec::new();
    }
    vec![
        "-c".to_string(),
        format!("url.{base}.insteadOf={GITHUB_HOST}"),
    ]
}

/// git insteadOf arguments for the active mirror.
pub fn git_clone_args(origin: &str) -> Vec<String> {
    let mirror = resolve_active_mirror();
    git_clone_args_with(&mirror, origin)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mirror(name: &str, git_instead_of_base: Option<&str>) -> ResolvedMirror {
        ResolvedMirror {
            name: name.to_string(),
            git_instead_of_base: git_instead_of_base.map(str::to_string),
        }
    }

    #[test]
    fn none_mirror_rewrites_nothing() {
        let none = ResolvedMirror::none();
        assert!(none.is_none());
        assert!(git_clone_args_with(&none, "https://github.com/o/r.git").is_empty());
    }

    #[test]
    fn git_instead_of_args_only_for_github_origins() {
        let custom = mirror("custom", Some("https://gitclone.example.com/github.com"));
        assert_eq!(
            git_clone_args_with(&custom, "https://github.com/ohmybash/oh-my-bash.git"),
            vec![
                "-c".to_string(),
                "url.https://gitclone.example.com/github.com.insteadOf=https://github.com/"
                    .to_string(),
            ]
        );
        // Non-GitHub origins get no insteadOf config.
        assert!(git_clone_args_with(&custom, "https://gitlab.com/o/r.git").is_empty());
        assert!(git_clone_args_with(&custom, "D:/local/repo").is_empty());
        assert!(git_clone_args_with(&custom, "git@github.com:o/r.git").is_empty());
    }

    #[test]
    fn base_normalization_for_pasted_urls() {
        assert_eq!(
            normalize_git_base("https://git.example.com/github.com").unwrap(),
            "https://git.example.com/github.com/"
        );
        assert_eq!(
            normalize_git_base("https://git.example.com/github.com/").unwrap(),
            "https://git.example.com/github.com/"
        );
        assert_eq!(
            normalize_git_base("  https://git.example.com/github.com/// ").unwrap(),
            "https://git.example.com/github.com/"
        );
        assert!(normalize_git_base("git.example.com").is_err());
        assert!(normalize_git_base("ftp://git.example.com").is_err());
        assert!(normalize_git_base("https://").is_err());
        assert!(normalize_git_base("").is_err());
    }

    // ── config-file behavior (env-scoped temp file) ────────────────────────

    /// `NIU_MIRRORS` is process-global; serialize against every other
    /// env-mutating test in the crate (sources.rs uses the same lock for
    /// `NIU_PLUGIN_SOURCES_ROOT`).
    fn with_temp_mirrors(test: impl FnOnce(&std::path::Path)) {
        let _guard = crate::test_support::PROCESS_STATE_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mirrors.toml");
        std::env::set_var("NIU_MIRRORS", &path);
        test(&path);
        std::env::remove_var("NIU_MIRRORS");
    }

    #[test]
    fn missing_config_means_direct() {
        with_temp_mirrors(|path| {
            assert!(!path.exists());
            assert!(resolve_active_mirror().is_none());
            assert!(git_clone_args("https://github.com/o/r.git").is_empty());
        });
    }

    #[test]
    fn malformed_config_degrades_to_direct() {
        with_temp_mirrors(|path| {
            fs::write(path, "this is [ not toml").unwrap();
            assert!(resolve_active_mirror().is_none());
            assert!(load_mirror_config().is_err());
        });
    }

    #[test]
    fn set_custom_git_mirror_roundtrips_and_rewrites() {
        with_temp_mirrors(|path| {
            let stored = set_custom_git_mirror("https://git.example.com/github.com").unwrap();
            assert_eq!(stored, "https://git.example.com/github.com/");
            let text = fs::read_to_string(path).unwrap();
            assert!(text.contains(MIRRORS_SCHEMA));
            assert!(text.contains("active = \"custom\""));
            // the caveat ships in the file, not as a preset
            assert!(text.contains("UNSUPPORTED"));
            let config = load_mirror_config().unwrap().unwrap();
            assert_eq!(config.active.as_deref(), Some("custom"));
            assert_eq!(
                config.github.unwrap().git_instead_of.as_deref(),
                Some("https://git.example.com/github.com/")
            );
            assert_eq!(
                git_clone_args("https://github.com/o/r.git"),
                vec![
                    "-c".to_string(),
                    "url.https://git.example.com/github.com/.insteadOf=https://github.com/"
                        .to_string()
                ]
            );
        });
    }

    #[test]
    fn set_direct_roundtrips_and_preserves_the_custom_section() {
        with_temp_mirrors(|_path| {
            set_custom_git_mirror("https://git.example.com/github.com/").unwrap();
            set_direct().unwrap();
            let config = load_mirror_config().unwrap().unwrap();
            assert_eq!(config.active.as_deref(), Some("none"));
            assert!(config.github.is_some(), "custom section preserved");
            assert!(resolve_active_mirror().is_none());
            assert!(git_clone_args("https://github.com/o/r.git").is_empty());
        });
    }

    /// A hand-edited file from the download era (prefix / releases channels)
    /// parses compatibly: the legacy keys are ignored, the git channel
    /// still resolves.
    #[test]
    fn legacy_download_channels_are_ignored() {
        with_temp_mirrors(|_path| {
            fs::write(
                mirrors_path(),
                concat!(
                    "schema = \"niubash:mirrors@0.1.0\"\n",
                    "active = \"custom\"\n",
                    "\n[github]\n",
                    "prefix = \"https://mirror.corp.cn/\"\n",
                    "git_instead_of = \"https://git.corp.cn/github.com/\"\n",
                    "\n[github.releases]\n",
                    "prefix = \"https://release.corp.cn/\"\n",
                ),
            )
            .unwrap();
            let mirror = resolve_active_mirror();
            assert_eq!(
                mirror.git_instead_of_base.as_deref(),
                Some("https://git.corp.cn/github.com/")
            );
            assert_eq!(
                git_clone_args("https://github.com/o/r.git"),
                vec![
                    "-c".to_string(),
                    "url.https://git.corp.cn/github.com/.insteadOf=https://github.com/".to_string()
                ]
            );
        });
    }

    #[test]
    fn unknown_active_name_degrades_to_direct() {
        with_temp_mirrors(|path| {
            fs::write(path, "active = \"dead-mirror\"").unwrap();
            assert!(resolve_active_mirror().is_none());
        });
    }

    #[test]
    fn set_custom_git_mirror_rejects_non_urls() {
        with_temp_mirrors(|_path| {
            assert!(set_custom_git_mirror("not a url").is_err());
            assert!(set_custom_git_mirror("").is_err());
        });
    }

    /// niubash#303: the shipped template's community examples must never
    /// recommend a mirror that has been dead for years — hub.fastgit.xyz
    /// 404s since ~2021 and still sat in every freshly-rendered
    /// mirrors.toml. Each replacement suggestion must be a live-service
    /// URL shape (gitclone.com and ghproxy.net were both verified with a
    /// real `git clone` through the insteadOf rewrite at edit time).
    #[test]
    fn rendered_template_suggests_only_live_mirrors() {
        with_temp_mirrors(|path| {
            set_direct().unwrap();
            let text = fs::read_to_string(path).unwrap();
            assert!(
                !text.contains("fastgit"),
                "template still recommends the dead hub.fastgit.xyz:\n{text}"
            );
            assert!(
                text.contains("#   git_instead_of = \"https://gitclone.com/github.com\""),
                "gitclone example missing:\n{text}"
            );
            assert!(
                text.contains("#   git_instead_of = \"https://ghproxy.net/github.com\""),
                "ghproxy.net example missing:\n{text}"
            );
        });
    }
}
