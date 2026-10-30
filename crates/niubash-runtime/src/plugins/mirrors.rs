//! Git-transport mirroring — the China-network comfort layer (design
//! §14.8; download retraction 2026-10-04: git-only). The former HTTPS
//! prefix-rewrite channel (ghproxy-class download proxies) and the
//! reachability probe retired with the download driver: niu carries zero
//! network/HTTP responsibility, so the only traffic left to mirror is
//! `git clone` / `git fetch`, rewritten through git's own `insteadOf`
//! mechanism at transport time.
//!
//! Model: one user-editable config file `~/.niubash/mirrors.json` holding
//! at most a `custom` mirror (a git insteadOf base) or `"active": "none"`
//! for direct connection. `niu plugin mirror set <url>` writes it in one
//! command; community mirror examples live in the plugin guide, not in
//! the file — they may die at any moment and are the user's to verify.
//!
//! Iron invariant (§14.8): **rewrites happen at the transport layer only.**
//! The spec (`plugins.json`), the lock (`registry.json`) and recipe URLs
//! keep canonical GitHub origins, so a tree installed through a mirror
//! stays identical to a direct install and the lockfile remains portable
//! across machines and networks. No auto-select, no
//! probing: the user pastes their own mirror URL.
//!
//! Degrade discipline: a missing file, unknown `active` name, or malformed
//! JSON degrades to direct connection in the transport (a mirror config
//! problem must never break a fetch); `niu plugin mirror list` and `niu
//! doctor` surface the condition instead. The config is JSON since the
//! 1.5.0 product-TOML deletion; the pre-1.5.0 TOML form (`mirrors.toml`,
//! same directory) is converted exactly once by
//! [`migrate_legacy_mirrors`] (`niu plugin migrate`) — no runtime path
//! reads it. Legacy `[github] prefix` / `[github.releases]` download
//! channels in a TOML-era file are dropped by that conversion — niu no
//! longer has a download transport for them to affect.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Context;
use serde::{Deserialize, Serialize};

use crate::path_utils::shell_home_dir;

pub const MIRRORS_SCHEMA: &str = "niubash:mirrors@0.1.0";

/// The canonical GitHub HTTPS origin eligible for rewriting. Everything
/// else (company GitLab, local paths, other hosts) passes through — a
/// mirror base must never silently capture traffic it does not proxy.
const GITHUB_HOST: &str = "https://github.com/";

/// Pre-1.5.0 TOML mirror config name. Read exactly once, by
/// [`migrate_legacy_mirrors`]; no runtime path looks for it.
const LEGACY_MIRRORS_FILE: &str = "mirrors.toml";

/// Where mirrors.json lives: `$NIU_MIRRORS` overrides (tests, portable
/// setups); default `~/.niubash/mirrors.json` (the spec.rs convention).
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
        .join("mirrors.json")
}

/// Where the pre-1.5.0 TOML mirror config lived: the sibling of
/// [`mirrors_path`] under its retired name (migrate-only).
fn legacy_mirrors_path() -> PathBuf {
    mirrors_path().with_file_name(LEGACY_MIRRORS_FILE)
}

/// The parsed mirrors.json. `Ok(None)` = no file yet (direct connection).
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

/// The user-editable custom mirror (the `github` object in mirrors.json).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct GithubMirror {
    /// git insteadOf base (gitclone class):
    /// `git -c url.<base>.insteadOf=https://github.com/`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_instead_of: Option<String>,
}

/// Parse mirrors.json text: blank (zero-byte or whitespace-only) yields
/// the default config, not a parse error — the tolerance the TOML reader
/// had for an empty `mirrors.toml` (an empty TOML document is a valid
/// all-default config; an empty JSON document is a serde EOF error),
/// restored for the JSON form. Non-blank garbage goes to serde_json
/// unchanged and keeps erroring. (Same rule as
/// `super::spec::parse_spec_json`.)
fn parse_mirror_config_json(text: &str) -> anyhow::Result<MirrorConfig> {
    if text.trim().is_empty() {
        return Ok(MirrorConfig::default());
    }
    Ok(serde_json::from_str(text)?)
}

pub fn load_mirror_config() -> anyhow::Result<Option<MirrorConfig>> {
    let path = mirrors_path();
    if !path.is_file() {
        return Ok(None);
    }
    let text = fs::read_to_string(&path)?;
    Ok(Some(parse_mirror_config_json(&text)?))
}

/// Render the config as the file users edit: pretty JSON with the schema
/// marker and the active channel always stamped (the roles the TOML
/// header lines used to play). JSON carries no comments, so the
/// "community mirror, may die anytime" caveat that used to ship as
/// comments in the file now lives in the plugin guide.
fn render_config(config: &MirrorConfig) -> anyhow::Result<String> {
    let mut rendered = config.clone();
    if rendered.schema.is_none() {
        rendered.schema = Some(MIRRORS_SCHEMA.to_string());
    }
    if rendered.active.is_none() {
        rendered.active = Some("none".to_string());
    }
    // An empty insteadOf base carries no section (the TOML renderer's
    // rule): the channel exists only once a base is set.
    if rendered
        .github
        .as_ref()
        .is_some_and(|github| github.git_instead_of.as_deref().is_none_or(str::is_empty))
    {
        rendered.github = None;
    }
    let mut body = serde_json::to_string_pretty(&rendered)?;
    body.push('\n');
    Ok(body)
}

fn write_mirror_config_to(path: &Path, config: &MirrorConfig) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, render_config(config)?)?;
    Ok(())
}

fn write_mirror_config(config: &MirrorConfig) -> anyhow::Result<()> {
    write_mirror_config_to(&mirrors_path(), config)
}

/// Outcome of [`migrate_legacy_mirrors`]: what the one-shot bridge moved.
#[derive(Debug, Clone)]
pub struct MirrorMigrationReport {
    /// The legacy config file, renamed aside (present iff one was found).
    pub backup: Option<PathBuf>,
    /// An existing mirrors.json was kept as-is (the config is a single
    /// indivisible document, so there is no field-wise merge: the JSON
    /// form wins wholesale and the legacy file is only renamed aside).
    pub kept_existing: bool,
    /// The legacy config, left in place because it could not be
    /// converted (named, with the reason — never guessed at).
    pub skipped: Vec<String>,
}

/// One-time migration from the pre-1.5.0 TOML mirror config
/// (`mirrors.toml`) to the JSON config (`mirrors.json`). This is the
/// ONLY code that still reads the TOML form — the runtime never does
/// (the same one-shot pattern as [`super::spec::migrate_legacy_spec`]).
/// The legacy file is renamed to `<name>.migrated-<UTC timestamp>`,
/// never deleted. When there is no legacy config this returns
/// `Ok(None)`; a config that does not parse as the legacy shape is
/// skipped and named in the report (left in place, fixable by hand,
/// re-runnable) while the rest of `niu plugin migrate` still proceeds.
/// The legacy `[github] prefix` / `[github.releases]` download channels
/// are not carried over — the serde model never had them (the transport
/// they fed retired in 2026-10). An existing JSON config wins wholesale;
/// one niu cannot read refuses the conversion before anything on disk
/// changes.
pub fn migrate_legacy_mirrors() -> anyhow::Result<Option<MirrorMigrationReport>> {
    let legacy_path = legacy_mirrors_path();
    let json_path = if legacy_path == mirrors_path() {
        // An override still naming the retired file itself: the JSON
        // form takes the same stem with the new extension.
        legacy_path.with_extension("json")
    } else {
        mirrors_path()
    };
    let text = match fs::read_to_string(&legacy_path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => {
            return Err(err).with_context(|| format!("could not read {}", legacy_path.display()))
        }
    };
    let legacy_config: MirrorConfig = match toml::from_str(&text) {
        Ok(config) => config,
        Err(err) => {
            return Ok(Some(MirrorMigrationReport {
                backup: None,
                kept_existing: false,
                skipped: vec![format!(
                    "{}: could not parse the legacy TOML mirror config ({err})",
                    legacy_path.display()
                )],
            }));
        }
    };
    if json_path.is_file() {
        let existing_text = fs::read_to_string(&json_path)?;
        let _: MirrorConfig = parse_mirror_config_json(&existing_text).with_context(|| {
            format!(
                "could not parse {} — fix or remove the file by hand; \
                 niu never rewrites a mirror config it cannot read",
                json_path.display()
            )
        })?;
        let backup = super::distros::rename_legacy_file(&legacy_path)?;
        return Ok(Some(MirrorMigrationReport {
            backup: Some(backup),
            kept_existing: true,
            skipped: Vec::new(),
        }));
    }
    write_mirror_config_to(&json_path, &legacy_config)?;
    let backup = super::distros::rename_legacy_file(&legacy_path)?;
    Ok(Some(MirrorMigrationReport {
        backup: Some(backup),
        kept_existing: false,
        skipped: Vec::new(),
    }))
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
/// `"active": "custom"` and the `github` object's `git_instead_of`. The
/// value is returned for the confirmation message.
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
/// design: missing file, malformed JSON, and unknown names all degrade to
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
        let path = dir.path().join("mirrors.json");
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
            fs::write(path, "this is { not json").unwrap();
            assert!(resolve_active_mirror().is_none());
            assert!(load_mirror_config().is_err());
        });
    }

    /// A blank mirrors.json (zero-byte or whitespace-only — the shape of
    /// a never-written or truncated file) reads as the default config:
    /// the TOML reader took an empty mirrors.toml as all-default, so the
    /// JSON reader must not turn the same file into a hard error for
    /// `mirror list` / doctor / the `mirror set` writers.
    #[test]
    fn blank_config_reads_as_default_direct() {
        with_temp_mirrors(|path| {
            fs::write(path, "").unwrap();
            let config = load_mirror_config()
                .unwrap()
                .expect("a blank file is present-but-empty, not missing");
            assert_eq!(config, MirrorConfig::default());
            assert!(resolve_active_mirror().is_none());
            assert!(git_clone_args("https://github.com/o/r.git").is_empty());
            // The write consumers proceed over it instead of erroring.
            set_direct().unwrap();
            assert_eq!(
                load_mirror_config().unwrap().unwrap().active.as_deref(),
                Some("none")
            );
            // Whitespace-only is the same shape.
            fs::write(path, " \n").unwrap();
            assert_eq!(
                load_mirror_config().unwrap().unwrap(),
                MirrorConfig::default()
            );
            assert!(resolve_active_mirror().is_none());
        });
    }

    /// The migrate bridge validates an existing mirrors.json before
    /// keeping it: a blank one is empty (kept, conversion proceeds),
    /// not a config niu "cannot read".
    #[test]
    fn migrate_keeps_a_blank_existing_json_config() {
        with_temp_mirrors(|path| {
            let legacy = path.with_file_name("mirrors.toml");
            fs::write(&legacy, "active = \"none\"\n").unwrap();
            fs::write(path, "").unwrap();
            let report = migrate_legacy_mirrors().unwrap().unwrap();
            assert!(report.kept_existing);
            assert!(report.skipped.is_empty());
            assert!(report.backup.is_some());
            assert!(!legacy.exists(), "legacy renamed aside");
        });
    }

    #[test]
    fn set_custom_git_mirror_roundtrips_and_rewrites() {
        with_temp_mirrors(|path| {
            let stored = set_custom_git_mirror("https://git.example.com/github.com").unwrap();
            assert_eq!(stored, "https://git.example.com/github.com/");
            let text = fs::read_to_string(path).unwrap();
            assert!(text.contains(MIRRORS_SCHEMA));
            assert!(text.contains("\"active\": \"custom\""));
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

    /// A hand-edited TOML file from the download era (prefix / releases
    /// channels) converts through the one-shot bridge: the legacy keys
    /// are dropped, the git channel still resolves. The runtime itself
    /// never reads the TOML form.
    #[test]
    fn legacy_toml_with_download_channels_migrates_and_drops_them() {
        with_temp_mirrors(|path| {
            let legacy = path.with_file_name("mirrors.toml");
            fs::write(
                &legacy,
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
            // Invisible to the runtime until the bridge runs.
            assert!(load_mirror_config().unwrap().is_none());
            let report = migrate_legacy_mirrors().unwrap().unwrap();
            assert!(!report.kept_existing);
            assert!(report.skipped.is_empty());
            let backup = report.backup.expect("legacy file renamed aside");
            assert!(backup.exists());
            assert!(!legacy.exists());
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
            // A second run is a no-op.
            assert!(migrate_legacy_mirrors().unwrap().is_none());
        });
    }

    #[test]
    fn unknown_active_name_degrades_to_direct() {
        with_temp_mirrors(|path| {
            fs::write(path, "{ \"active\": \"dead-mirror\" }").unwrap();
            assert!(resolve_active_mirror().is_none());
        });
    }

    /// A legacy config that does not parse is named and left in place;
    /// no JSON is written (the spec face's named-skip discipline).
    #[test]
    fn migrate_skips_an_unparseable_legacy_config() {
        with_temp_mirrors(|path| {
            let legacy = path.with_file_name("mirrors.toml");
            fs::write(&legacy, "this is [ not toml").unwrap();
            let report = migrate_legacy_mirrors().unwrap().unwrap();
            assert!(report.backup.is_none());
            assert_eq!(report.skipped.len(), 1);
            assert!(legacy.exists(), "skipped file stays put");
            assert!(!path.exists(), "no JSON written for a skipped config");
        });
    }

    /// An existing JSON config wins wholesale: the legacy file is only
    /// renamed aside, the JSON content is untouched.
    #[test]
    fn migrate_keeps_an_existing_json_config() {
        with_temp_mirrors(|path| {
            set_custom_git_mirror("https://json.example.com/github.com").unwrap();
            let before = fs::read_to_string(path).unwrap();
            let legacy = path.with_file_name("mirrors.toml");
            fs::write(
                &legacy,
                "active = \"custom\"\n\n[github]\ngit_instead_of = \"https://toml.example.com/github.com/\"\n",
            )
            .unwrap();
            let report = migrate_legacy_mirrors().unwrap().unwrap();
            assert!(report.kept_existing);
            assert!(report.backup.is_some());
            assert!(!legacy.exists());
            assert_eq!(fs::read_to_string(path).unwrap(), before);
            assert_eq!(
                resolve_active_mirror().git_instead_of_base.as_deref(),
                Some("https://json.example.com/github.com/")
            );
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
    /// mirrors.toml. The JSON render carries no examples at all (JSON has
    /// no comments; the examples moved to the plugin guide), so the
    /// rendered file can never embed a dead — or any — mirror URL.
    #[test]
    fn rendered_config_embeds_no_mirror_examples() {
        with_temp_mirrors(|path| {
            set_direct().unwrap();
            let text = fs::read_to_string(path).unwrap();
            assert!(
                !text.contains("fastgit"),
                "render still recommends the dead hub.fastgit.xyz:\n{text}"
            );
            assert!(
                !text.contains("git_instead_of"),
                "render embeds a mirror example:\n{text}"
            );
            assert!(text.contains(MIRRORS_SCHEMA));
            assert!(load_mirror_config().unwrap().is_some());
        });
    }
}
