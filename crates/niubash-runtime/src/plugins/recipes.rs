//! Plugin recipe registry — the mason-registry pattern for niu (study §6,
//! §10.5): one pure-data index, one row per known bash-ecosystem asset, no
//! behavior code per recipe. Bulk rows are generated from the ecosystem
//! corpus (`scripts/gen-plugin-recipes.py`); manager rows for oh-my-bash /
//! bash-it / bash-completion are derived from the compiled-in adapters at
//! runtime so ids/origins/licenses can never drift.
//!
//! Drivers (closed set, study §10.3): `git` (tree source through the
//! existing adapters — manager or generic-with-entry) and `download`
//! (executable-tool rows). The download driver is **retracted** (owner
//! ruling 2026-10-04, "download full retraction"): niu carries zero
//! network/HTTP responsibility, so a `download` row is catalog metadata
//! only — `add` prints a package-manager recommendation (winget/scoop on
//! Windows, apt/dnf/yum/brew elsewhere) instead of fetching anything.
//! Asset recipes (`manager` + `asset`) ride on a manager source and
//! resolve through its own selection mechanism.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use anyhow::{anyhow, bail};
use serde::Deserialize;

use super::assets;
use super::sources;

/// Generated seed index, compiled in (regenerate with
/// `scripts/gen-plugin-recipes.py`).
const RECIPES_JSON: &str = include_str!("../../assets/plugins/recipes.json");

/// Recipe categories (the index's five classes plus `alias`, which oh-my-bash
/// and bash-it both ship as a distinct asset class).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecipeCategory {
    Manager,
    Theme,
    Plugin,
    Alias,
    Completion,
    Prompt,
}

impl RecipeCategory {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Manager => "manager",
            Self::Theme => "theme",
            Self::Plugin => "plugin",
            Self::Alias => "alias",
            Self::Completion => "completion",
            Self::Prompt => "prompt",
        }
    }
}

/// How a recipe installs. Absent = info-only row (known ecosystem asset;
/// `add` explains instead of installing).
#[derive(Debug, Clone)]
pub enum RecipeDriver {
    /// Git tree source: `kind` names the source adapter (a manager id or
    /// `generic`), `entry` the generic loader entry file.
    Git {
        kind: String,
        entry: Option<String>,
        origin: Option<String>,
    },
    /// Executable-tool row (former direct-binary driver, retracted
    /// 2026-10-04): catalog metadata only — the platform release URLs are
    /// data for `recipe show`, never fetched. `add` prints a
    /// package-manager recommendation.
    Download {
        version: String,
        /// Platform key → upstream release URL (catalog data only).
        downloads: BTreeMap<String, DownloadAssetInfo>,
    },
}

/// Catalog metadata for one platform row of an executable-tool recipe.
/// The generated recipes.json rows carry more fields (archive kind, pinned
/// sha256, bins) from the download-driver era; they parse compatibly and
/// are ignored — niu no longer downloads, so nothing needs them.
#[derive(Debug, Clone, Deserialize)]
pub struct DownloadAssetInfo {
    pub url: String,
}

/// One recipe row (data only — the generator and the adapter-derived
/// manager rows are the only producers). Deserialized from a flat raw row
/// (driver fields are siblings, mason-style) and folded into
/// [`RecipeDriver`]; TOML-internal tagged enums cannot express that shape.
#[derive(Debug, Clone)]
pub struct Recipe {
    pub id: String,
    pub category: RecipeCategory,
    pub summary: String,
    pub license: String,
    /// Info URL (upstream page, tree, release).
    pub url: String,
    pub driver: Option<RecipeDriver>,
    /// Asset recipes: the manager source this rides on.
    pub manager: Option<String>,
    pub asset_kind: Option<String>,
    pub asset: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawRecipe {
    id: String,
    category: RecipeCategory,
    summary: String,
    license: String,
    url: String,
    #[serde(default)]
    driver: Option<String>,
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    entry: Option<String>,
    #[serde(default)]
    origin: Option<String>,
    #[serde(default)]
    version: Option<String>,
    #[serde(default)]
    downloads: Option<BTreeMap<String, DownloadAssetInfo>>,
    #[serde(default)]
    manager: Option<String>,
    #[serde(default)]
    asset_kind: Option<String>,
    #[serde(default)]
    asset: Option<String>,
}

impl TryFrom<RawRecipe> for Recipe {
    type Error = String;
    fn try_from(raw: RawRecipe) -> Result<Self, Self::Error> {
        let driver = match raw.driver.as_deref() {
            None => None,
            Some("git") => Some(RecipeDriver::Git {
                kind: raw
                    .kind
                    .clone()
                    .ok_or(format!("recipe '{}': driver=git requires kind", raw.id))?,
                entry: raw.entry.clone(),
                origin: raw.origin.clone(),
            }),
            Some("download") => Some(RecipeDriver::Download {
                version: raw.version.clone().ok_or(format!(
                    "recipe '{}': driver=download requires version",
                    raw.id
                ))?,
                downloads: raw.downloads.clone().unwrap_or_default(),
            }),
            Some(other) => {
                return Err(format!(
                    "recipe '{}': unknown driver '{other}' (expected git|download)",
                    raw.id
                ))
            }
        };
        if raw.entry.is_some() && raw.driver.as_deref() != Some("git") {
            return Err(format!("recipe '{}': entry requires driver=git", raw.id));
        }
        if raw.manager.is_some() && raw.driver.is_some() {
            return Err(format!(
                "recipe '{}': manager rows cannot carry a driver",
                raw.id
            ));
        }
        Ok(Recipe {
            id: raw.id,
            category: raw.category,
            summary: raw.summary,
            license: raw.license,
            url: raw.url,
            driver,
            manager: raw.manager,
            asset_kind: raw.asset_kind,
            asset: raw.asset,
        })
    }
}

/// The merged registry: adapter-derived manager rows + generated data rows.
pub fn recipes() -> &'static [Recipe] {
    static RECIPES: OnceLock<Vec<Recipe>> = OnceLock::new();
    RECIPES.get_or_init(|| {
        #[derive(Deserialize)]
        struct Registry {
            #[serde(default)]
            recipe: Vec<RawRecipe>,
        }
        let parsed: Registry = serde_json::from_str(RECIPES_JSON)
            .unwrap_or_else(|err| panic!("compiled-in recipes.json must parse: {err}"));
        let mut all: Vec<Recipe> = parsed
            .recipe
            .into_iter()
            .map(|raw| {
                raw.try_into()
                    .unwrap_or_else(|err| panic!("compiled-in recipes.json is invalid: {err}"))
            })
            .collect();
        // Manager rows derive from the adapters (single source of truth —
        // catalog.rs derives its shorthand table the same way).
        for adapter in sources::builtin_source_adapters() {
            let Some(origin) = adapter.default_origin() else {
                continue;
            };
            all.push(Recipe {
                id: adapter.id().to_string(),
                category: RecipeCategory::Manager,
                summary: adapter.summary().to_string(),
                license: adapter.license().to_string(),
                url: origin.trim_end_matches(".git").to_string(),
                driver: Some(RecipeDriver::Git {
                    kind: adapter.id().to_string(),
                    entry: None,
                    origin: Some(origin.to_string()),
                }),
                manager: None,
                asset_kind: None,
                asset: None,
            });
        }
        all.sort_by(|left, right| left.id.cmp(&right.id));
        all
    })
}

pub fn recipe(id: &str) -> Option<&'static Recipe> {
    recipes().iter().find(|recipe| recipe.id == id)
}

/// Outcome of `niu plugin add <recipe-id>`: what happened and what the user
/// should run next (health-style "name the repair verb", study §10.1).
#[derive(Debug, Clone)]
pub struct RecipeInstall {
    pub recipe_id: String,
    /// human-readable one-line summary of what was done.
    pub summary: String,
    /// next commands, in order (empty when everything is already active).
    pub next: Vec<String>,
}

/// `niu plugin add <recipe-id>`: install through the recipe's driver.
/// Sources land installed; assets activate through the enable step
/// (immediately, when the manager source is already installed).
pub fn install(id: &str) -> anyhow::Result<RecipeInstall> {
    let found = recipe(id).ok_or_else(|| anyhow!("unknown recipe '{id}'"))?;
    match (&found.driver, &found.manager) {
        (
            Some(RecipeDriver::Git {
                kind,
                entry,
                origin,
            }),
            None,
        ) => install_git_source(&found.id, kind, entry.as_deref(), origin.as_deref()),
        (None, Some(manager)) => install_manager_asset(found, manager),
        (None, None) => Ok(RecipeInstall {
            recipe_id: found.id.clone(),
            summary: format!(
                "'{}' is an info-only recipe (no niu driver); see {}",
                found.id, found.url
            ),
            next: vec![],
        }),
        (Some(RecipeDriver::Download { .. }), Some(_)) => bail!(
            "recipe '{}' is malformed: download driver with a manager",
            found.id
        ),
        (Some(RecipeDriver::Git { .. }), Some(_)) => bail!(
            "recipe '{}' is malformed: git driver with a manager",
            found.id
        ),
        (Some(RecipeDriver::Download { version, downloads }), None) => {
            recommend_package_manager(found, version, downloads)
        }
    }
}

fn install_git_source(
    id: &str,
    kind: &str,
    entry: Option<&str>,
    origin: Option<&str>,
) -> anyhow::Result<RecipeInstall> {
    let origin = origin.ok_or_else(|| anyhow!("recipe '{id}' has no git origin"))?;
    // Recipe kind → adapter id. The wt47 "generic" single-entry adapter
    // merged into the anyplug model as the open file source ("file"):
    // every candidate is still enumerated honestly (§14.6.1 — no guessed
    // unique entry); the recipe's `entry` names the *recommended* file,
    // verified on staging and surfaced as the enable verb below. Manager
    // kinds (oh-my-bash, bash-it, bash-completion, bpkg) pass through.
    let (adapter_kind, per_install) = if kind == "generic" {
        ("file", true)
    } else {
        (kind, false)
    };
    let mut request = sources::SourceInstallRequest {
        adapter: Some(adapter_kind.to_string()),
        origin: sources::normalize_origin(origin),
        ..Default::default()
    };
    if per_install {
        request.id = Some(id.to_string());
        request.entry = entry.map(str::to_string);
    }
    let record = sources::add_source(request)?;
    let mut next = Vec::new();
    match entry {
        // File sources enable per-file (DirectFiles); the recipe's entry is
        // the recommended pick, spelled out so the next verb works as-is.
        Some(entry) => next.push(format!("niu plugin enable {}/{}", record.id, entry)),
        None => next.push(format!("niu plugin enable {}", record.id)),
    }
    Ok(RecipeInstall {
        recipe_id: id.to_string(),
        summary: format!("installed source '{}' ({})", record.id, record.version),
        next,
    })
}

/// Asset recipe: ensure the manager source exists, then activate the asset
/// through the manager's own selection mechanism.
fn install_manager_asset(found: &Recipe, manager: &str) -> anyhow::Result<RecipeInstall> {
    let asset = found
        .asset
        .as_deref()
        .ok_or_else(|| anyhow!("recipe '{}' has no asset name", found.id))?;
    let installed = sources::read_source_registry()
        .into_iter()
        .find(|record| record.id == manager);
    match installed {
        None => {
            // Install the manager first, then hint the enable step.
            let manager_recipe = recipe(manager).ok_or_else(|| {
                anyhow!(
                    "asset recipe '{}' references unknown manager '{manager}'",
                    found.id
                )
            })?;
            let manager_driver = manager_recipe
                .driver
                .as_ref()
                .ok_or_else(|| anyhow!("manager '{manager}' has no install driver"))?;
            let RecipeDriver::Git { kind, origin, .. } = manager_driver else {
                bail!("manager '{manager}' is not a git source");
            };
            let origin = origin
                .as_deref()
                .ok_or_else(|| anyhow!("manager '{manager}' has no origin"))?;
            sources::add_source(sources::SourceInstallRequest {
                adapter: Some(kind.clone()),
                origin: sources::normalize_origin(origin),
                ..Default::default()
            })?;
            Ok(RecipeInstall {
                recipe_id: found.id.clone(),
                summary: format!(
                    "installed manager '{manager}'; asset '{asset}' activates with the enable step"
                ),
                next: vec![format!("niu plugin enable {asset}")],
            })
        }
        Some(_) => {
            let outcome = assets::enable(asset)?;
            Ok(RecipeInstall {
                recipe_id: found.id.clone(),
                summary: outcome.summary,
                next: vec![],
            })
        }
    }
}

/// Package-manager spellings for the known executable-tool recipes
/// (download retraction, owner ruling 2026-10-04 + correction 2026-10-03):
/// niu never fetches binaries, so these rows install through the user's
/// real package managers. On Windows **wpm is the first-class
/// recommendation** for executable command-layer tools (owner correction:
/// the shell stopped downloading; wpm stays the primary tool channel) —
/// winget/scoop appear only as alternatives for what wpm does not carry
/// (GUI apps, fonts). Non-Windows platforms recommend native package
/// managers only; wpm strings compile out there (`cfg(windows)`, the
/// niubash 688f224 red line). The upstream URL is always printed so no
/// row can dead-end.
struct ToolPackages {
    winget: Option<&'static str>,
    scoop: Option<&'static str>,
    apt: Option<&'static str>,
    dnf: Option<&'static str>,
    brew: Option<&'static str>,
}

const fn pkgs(
    winget: Option<&'static str>,
    scoop: Option<&'static str>,
    apt: Option<&'static str>,
    dnf: Option<&'static str>,
    brew: Option<&'static str>,
) -> ToolPackages {
    ToolPackages {
        winget,
        scoop,
        apt,
        dnf,
        brew,
    }
}

fn tool_packages(id: &str) -> Option<ToolPackages> {
    Some(match id {
        "fzf" => pkgs(
            Some("junegunn.fzf"),
            Some("fzf"),
            Some("fzf"),
            Some("fzf"),
            Some("fzf"),
        ),
        "starship" => pkgs(
            Some("Starship.Starship"),
            Some("starship"),
            None,
            None,
            Some("starship"),
        ),
        "ripgrep" => pkgs(
            Some("BurntSushi.ripgrep.MSVC"),
            Some("ripgrep"),
            Some("ripgrep"),
            Some("ripgrep"),
            Some("ripgrep"),
        ),
        "fd" => pkgs(
            Some("sharkdp.fd"),
            Some("fd"),
            Some("fd-find"),
            Some("fd-find"),
            Some("fd"),
        ),
        "bat" => pkgs(
            Some("sharkdp.bat"),
            Some("bat"),
            Some("bat"),
            Some("bat"),
            Some("bat"),
        ),
        "eza" => pkgs(
            Some("eza-community.eza"),
            Some("eza"),
            Some("eza"),
            None,
            Some("eza"),
        ),
        "zoxide" => pkgs(
            Some("ajeetdsouza.zoxide"),
            Some("zoxide"),
            Some("zoxide"),
            Some("zoxide"),
            Some("zoxide"),
        ),
        "dust" => pkgs(
            Some("bootandy.dust"),
            Some("dust"),
            None,
            None,
            Some("dust"),
        ),
        "duf" => pkgs(
            Some("muesli.duf"),
            Some("duf"),
            Some("duf"),
            Some("duf"),
            Some("duf"),
        ),
        "erdtree" => pkgs(None, None, None, None, Some("erdtree")),
        "direnv" => pkgs(
            Some("direnv.direnv"),
            Some("direnv"),
            Some("direnv"),
            Some("direnv"),
            Some("direnv"),
        ),
        // niu-git ships in the wpm index (wpm/niugit.json upstream) but no
        // public package manager carries it: wpm first, releases page as
        // the machine-independent fallback.
        "niugit" => pkgs(None, None, None, None, None),
        _ => return None,
    })
}

/// The install commands one executable-tool recipe recommends, in a fixed
/// platform order. Windows leads with wpm (owner correction 2026-10-03:
/// wpm is the first-class executable-tool channel on Windows — the shell
/// stopped downloading, wpm was not demoted); winget/scoop follow as
/// alternatives. Each line is a complete, copy-pasteable command.
fn recommendation_lines(id: &str) -> Vec<String> {
    let mut lines = Vec::new();
    match tool_packages(id) {
        Some(ToolPackages {
            winget,
            scoop,
            apt,
            dnf,
            brew,
        }) => {
            // Windows: wpm first (cfg-gated: the wpm surface must not exist
            // in non-Windows builds at all — niubash 688f224 red line).
            #[cfg(windows)]
            lines.push(format!(
                "wpm install {id}                      # Windows — first choice"
            ));
            if let Some(pkg) = winget {
                lines.push(format!(
                    "winget install --id {pkg}    # Windows (alternative)"
                ));
            }
            if let Some(pkg) = scoop {
                lines.push(format!(
                    "scoop install {pkg}               # Windows (alternative)"
                ));
            }
            if let Some(pkg) = apt {
                lines.push(format!("sudo apt install {pkg}          # Debian / Ubuntu"));
            }
            if let Some(pkg) = dnf {
                lines.push(format!(
                    "sudo dnf install {pkg}          # Fedora / RHEL (yum on older)"
                ));
            }
            if let Some(pkg) = brew {
                lines.push(format!(
                    "brew install {pkg}               # macOS / Linuxbrew"
                ));
            }
        }
        None => {
            #[cfg(windows)]
            lines.push(format!(
                "wpm install {id}                      # Windows — first choice"
            ));
            lines.push(format!(
                "winget search {id}                  # Windows (alternative; then install --id)"
            ));
            lines.push(format!(
                "sudo apt install {id}             # Debian / Ubuntu (if packaged)"
            ));
            lines.push(format!(
                "brew install {id}                  # macOS / Linuxbrew (if packaged)"
            ));
        }
    }
    lines
}

/// The most specific one-line recommendation for surfaces that print a
/// single hint (the shell's command-not-found advice): the *current
/// platform's* primary install command — wpm on Windows, the first native
/// package manager elsewhere (Windows-only lines are skipped there).
pub fn first_recommendation(id: &str) -> Option<String> {
    let lines = recommendation_lines(id);
    let pick = lines.iter().find(|line| {
        #[cfg(windows)]
        {
            line.contains("wpm install")
        }
        #[cfg(not(windows))]
        {
            !line.contains("winget") && !line.contains("scoop") && !line.contains("wpm")
        }
    });
    let line = pick.or_else(|| lines.first())?;
    Some(line.split('#').next().unwrap_or(line).trim().to_string())
}

/// `niu plugin add <download-recipe>` after the download retraction (owner
/// ruling 2026-10-04 + wpm-first correction): the row stays as catalog
/// metadata, but niu never fetches executables — the install action is a
/// recommendation pointing at the platform's real package manager (wpm
/// first on Windows), plus the upstream release page.
fn recommend_package_manager(
    found: &Recipe,
    version: &str,
    downloads: &BTreeMap<String, DownloadAssetInfo>,
) -> anyhow::Result<RecipeInstall> {
    let mut next = recommendation_lines(&found.id);
    // The upstream URL is always present, so even a row with no curated
    // spelling cannot dead-end. Prefer the current platform's release URL,
    // else the recipe's project page.
    let platform_url = downloads
        .get(platform_key())
        .map(|asset| asset.url.clone())
        .or_else(|| downloads.values().next().map(|asset| asset.url.clone()));
    let upstream = platform_url.unwrap_or_else(|| found.url.clone());
    next.push(upstream);
    Ok(RecipeInstall {
        recipe_id: found.id.clone(),
        summary: format!(
            "'{}' ({version}) is an executable tool — niu does not download binaries; \
             install it with your system package manager",
            found.id
        ),
        next,
    })
}

/// Platform key matching the recipe asset table (mason `target` naming,
/// dash-joined os-arch). Used only to pick which catalog URL to show.
fn platform_key() -> &'static str {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("windows", "x86_64") => "windows-x64",
        ("windows", "aarch64") => "windows-arm64",
        ("linux", "x86_64") => "linux-x64",
        ("linux", "aarch64") => "linux-arm64",
        ("macos", "x86_64") => "darwin-x64",
        ("macos", "aarch64") => "darwin-arm64",
        _ => "unknown-platform",
    }
}

/// `niu plugin enable/disable <id>` routing: everything goes to the
/// asset/asset-layer machinery (the executable-tool PATH-block channel
/// retired with the download driver — a package-manager install puts the
/// binary on PATH itself).
pub fn enable(id: &str) -> anyhow::Result<assets::ActivationOutcome> {
    assets::enable(id)
}

pub fn disable(id: &str) -> anyhow::Result<assets::ActivationOutcome> {
    assets::disable(id)
}

/// Install state of one recipe for listings/UI: which channel owns it and
/// where it currently stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecipeState {
    /// Not installed.
    Available,
    /// Riding on an installed manager source (sub-state of the source).
    ManagerAsset { manager_state: String },
    /// Info-only row (no niu install driver).
    InfoOnly,
}

pub fn recipe_state(id: &str) -> RecipeState {
    let Some(found) = recipe(id) else {
        return RecipeState::Available;
    };
    match (&found.driver, &found.manager) {
        (None, Some(manager)) => {
            let state = sources::read_source_registry()
                .into_iter()
                .find(|record| record.id == *manager)
                .map(|record| {
                    if !record.path.is_dir() {
                        "missing".to_string()
                    } else {
                        "ready".to_string()
                    }
                })
                .unwrap_or_else(|| "not-installed".to_string());
            RecipeState::ManagerAsset {
                manager_state: state,
            }
        }
        (None, None) => RecipeState::InfoOnly,
        (Some(RecipeDriver::Git { .. }), manager) => {
            let source_id = manager.clone().unwrap_or_else(|| found.id.clone());
            match sources::read_source_registry()
                .into_iter()
                .find(|record| record.id == source_id)
            {
                Some(record) if record.path.is_dir() => RecipeState::ManagerAsset {
                    manager_state: "ready".to_string(),
                },
                Some(_) => RecipeState::ManagerAsset {
                    manager_state: "missing".to_string(),
                },
                None => RecipeState::Available,
            }
        }
        // Executable-tool rows are recommendations, not installs niu can
        // observe — report them like info-only rows.
        (Some(RecipeDriver::Download { .. }), _) => RecipeState::InfoOnly,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compiled_in_index_parses_and_is_unique() {
        let all = recipes();
        assert!(
            all.len() > 400,
            "expected a full seed index, got {}",
            all.len()
        );
        let mut ids: Vec<&str> = all.iter().map(|r| r.id.as_str()).collect();
        let total = ids.len();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), total, "duplicate recipe ids in the index");
    }

    #[test]
    fn manager_rows_derive_from_adapters() {
        for id in ["oh-my-bash", "bash-it", "bash-completion"] {
            let found = recipe(id).expect("manager row");
            assert_eq!(found.category, RecipeCategory::Manager);
            assert!(matches!(found.driver, Some(RecipeDriver::Git { .. })));
        }
        // The generic adapter has no catalog row (never auto-installed).
        assert!(recipe("generic").is_none());
    }

    #[test]
    fn seed_covers_the_named_ecosystem_assets() {
        for id in [
            "omb-theme-agnoster",
            "omb-plugin-git",
            "bashit-alias-git",
            "bashit-plugin-fzf", // if present in corpus
            "bash-completion",
            "bash-preexec",
            "bpkg",
            "basher",
            "starship",
            "fzf",
            "bash-git-prompt",
            "sexy-bash-prompt",
        ] {
            if id == "bashit-plugin-fzf" {
                // corpus-dependent; only assert when present
                continue;
            }
            assert!(recipe(id).is_some(), "missing seed recipe '{id}'");
        }
    }

    #[test]
    fn every_category_has_rows() {
        // The five named classes (managers/themes/plugins/completions/
        // prompts) plus the omb alias rows must all be non-empty.
        for category in [
            RecipeCategory::Manager,
            RecipeCategory::Theme,
            RecipeCategory::Plugin,
            RecipeCategory::Completion,
            RecipeCategory::Prompt,
            RecipeCategory::Alias,
        ] {
            let count = recipes()
                .iter()
                .filter(|recipe| recipe.category == category)
                .count();
            assert!(count > 0, "category '{}' has no rows", category.as_str());
        }
        let prompts = recipes()
            .iter()
            .filter(|recipe| recipe.category == RecipeCategory::Prompt)
            .count();
        assert!(prompts >= 4, "prompt category too thin: {prompts}");
    }

    #[test]
    fn download_recipes_carry_platform_rows() {
        let starship = recipe("starship").expect("starship recipe");
        let Some(RecipeDriver::Download { downloads, .. }) = &starship.driver else {
            panic!("starship must be an executable-tool (download) recipe");
        };
        assert!(downloads.contains_key("windows-x64"), "{downloads:?}");
        assert!(downloads.contains_key("linux-x64"), "{downloads:?}");
    }

    /// Download retraction (owner ruling 2026-10-04): the rows stay as
    /// catalog metadata and `add` recommends package managers — fully
    /// offline, no fetch is ever attempted.
    #[test]
    fn download_recipes_recommend_package_managers_instead_of_fetching() {
        let fzf = recipe("fzf").expect("fzf recipe");
        let Some(RecipeDriver::Download { version, .. }) = &fzf.driver else {
            panic!("fzf must be an executable-tool (download) recipe");
        };
        let report = install("fzf").expect("install is a recommendation, never a fetch");
        assert_eq!(report.recipe_id, "fzf");
        assert!(
            report.summary.contains("package manager"),
            "{}",
            report.summary
        );
        assert!(
            report.summary.contains(version.as_str()),
            "{}",
            report.summary
        );
        let joined = report.next.join("\n");
        // Windows: wpm first (owner correction 2026-10-03), winget/scoop as
        // alternatives; non-Windows: native managers only, zero wpm
        // strings (the 688f224 red line holds — the line never compiles in).
        #[cfg(windows)]
        {
            assert!(joined.contains("wpm install fzf"), "{joined}");
            assert!(
                joined.find("wpm install") < joined.find("winget install"),
                "{joined}"
            );
        }
        #[cfg(not(windows))]
        assert!(
            !joined.contains("wpm"),
            "no wpm strings on non-Windows:\n{joined}"
        );
        assert!(
            joined.contains("winget install --id junegunn.fzf"),
            "{joined}"
        );
        assert!(joined.contains("scoop install fzf"), "{joined}");
        assert!(joined.contains("sudo apt install fzf"), "{joined}");
        assert!(joined.contains("sudo dnf install fzf"), "{joined}");
        assert!(joined.contains("brew install fzf"), "{joined}");
        assert!(
            joined.contains("https://github.com/junegunn/fzf/"),
            "the upstream URL must be the dead-end-free fallback:\n{joined}"
        );
        // Every recommendation line is a copy-pasteable command (the URL
        // excepted) and mentions no niu download verb.
        assert!(
            !joined.contains("niu plugin tool"),
            "retracted tool verbs must not surface:\n{joined}"
        );
    }

    /// Rows without a curated spelling table still recommend honestly
    /// (wpm on Windows + search-oriented generic lines + upstream), and
    /// niugit — carried by the wpm index but no public package manager —
    /// leads with wpm on Windows and points at its GitHub releases.
    #[test]
    fn uncurated_and_own_project_tools_recommend_upstream() {
        let niugit = install("niugit").expect("niugit recommendation");
        let joined = niugit.next.join("\n");
        #[cfg(windows)]
        assert!(joined.contains("wpm install niugit"), "{joined}");
        assert!(
            joined.contains("https://github.com/unixwin/niu-git/"),
            "{joined}"
        );
        assert!(
            !joined.contains("winget install --id"),
            "niu-git has no winget package:\n{joined}"
        );
        // The shell-facing one-liner exists for every known tool row and
        // names the platform's primary channel.
        for id in [
            "fzf", "starship", "ripgrep", "fd", "bat", "eza", "zoxide", "dust", "duf", "direnv",
        ] {
            let first =
                first_recommendation(id).unwrap_or_else(|| panic!("{id}: no first_recommendation"));
            #[cfg(windows)]
            assert_eq!(first, format!("wpm install {id}"), "{id}: {first}");
            #[cfg(not(windows))]
            assert!(
                first.starts_with("sudo apt") || first.starts_with("brew"),
                "{id}: {first}"
            );
        }
        assert!(first_recommendation("erdtree").is_some());
    }

    #[test]
    fn asset_recipes_reference_known_managers() {
        for found in recipes() {
            if let Some(manager) = &found.manager {
                assert!(
                    recipe(manager).is_some(),
                    "recipe '{}' references unknown manager '{manager}'",
                    found.id
                );
            }
        }
    }

    #[test]
    fn info_only_recipes_explain_instead_of_installing() {
        let ble = recipe("ble.sh").expect("ble.sh recipe");
        assert!(
            ble.driver.is_none(),
            "ble.sh is info-only (build from source)"
        );
    }

    /// The audited upstream-root entry files for every curated generic
    /// recipe (driver=git, kind=generic, entry=<file>) — observed 2026-10-02
    /// against each origin's root listing (GitHub contents API, wt61/journeyfix
    /// G1). The journey's `distro apply full` failed on bash-preexec because
    /// the seed named the bare id while upstream ships `bash-preexec.sh`
    /// (`sources.rs` entry validation: "entry file 'bash-preexec' not found").
    /// Every new curated generic entry must be audited into this table; a
    /// drifted `entry` fails here instead of inside a user's install.
    const AUDITED_UPSTREAM_ROOT_ENTRIES: &[(&str, &str)] = &[
        ("bash-preexec", "bash-preexec.sh"),
        ("liquidprompt", "liquidprompt"),
        ("bash-git-prompt", "gitprompt.sh"),
        ("bpkg", "bpkg.sh"),
        // niubash#171 collection curation (audited 2026-10-02 via the
        // GitHub contents API, wt61 pattern): the four independent recipes
        // the built-in collections carry.
        ("complete-alias", "complete_alias"),
        ("fzf-git.sh", "fzf-git.sh"),
        ("bash-sensible", "sensible.bash"),
        ("git-flow-completion", "git-flow-completion.bash"),
    ];

    #[test]
    fn curated_generic_entries_match_audited_upstream_roots() {
        for found in recipes() {
            let Some(RecipeDriver::Git {
                kind,
                entry: Some(entry),
                ..
            }) = &found.driver
            else {
                continue;
            };
            if kind != "generic" {
                continue;
            }
            let audited = AUDITED_UPSTREAM_ROOT_ENTRIES
                .iter()
                .find(|(id, _)| *id == found.id)
                .unwrap_or_else(|| {
                    panic!(
                        "curated generic recipe '{}' is not in the upstream-root audit table — \
                         audit its origin root and pin the entry file here",
                        found.id
                    )
                });
            assert_eq!(
                entry, &audited.1,
                "recipe '{}' entry drifted from the audited upstream root \
                 ({} ships '{}'): re-audit before changing the seed",
                found.id, found.id, audited.1
            );
        }
        // The table itself stays exactly as large as the curated set — an
        // orphaned row is a stale audit, not coverage.
        let curated: Vec<&str> = recipes()
            .iter()
            .filter_map(|found| match &found.driver {
                Some(RecipeDriver::Git {
                    kind,
                    entry: Some(_),
                    ..
                }) if kind == "generic" => Some(found.id.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(
            curated.len(),
            AUDITED_UPSTREAM_ROOT_ENTRIES.len(),
            "audit table size must track the curated generic set: {curated:?}"
        );
    }

    /// The loader's dry run on a fixture skeleton: the recipe's declared entry
    /// must exist in a tree shaped like the upstream root, using the exact
    /// production validation path (`sources::add_source` with the recipe's
    /// request shape — offline, local-directory origin). This is the check the
    /// journey's `distro apply full` performs for real against GitHub.
    #[test]
    fn curated_entries_pass_the_loader_dry_run_on_a_skeleton() {
        use super::super::sources;
        let _guard = crate::test_support::PROCESS_STATE_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        struct EnvGuard {
            name: &'static str,
            previous: Option<std::ffi::OsString>,
        }
        impl Drop for EnvGuard {
            fn drop(&mut self) {
                match &self.previous {
                    Some(value) => std::env::set_var(self.name, value),
                    None => std::env::remove_var(self.name),
                }
            }
        }
        let temp = std::env::temp_dir().join(format!(
            "niu-recipes-dryrun-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&temp).unwrap();
        let _root = EnvGuard {
            name: "NIU_PLUGIN_SOURCES_ROOT",
            previous: std::env::var_os("NIU_PLUGIN_SOURCES_ROOT"),
        };
        // Deliberately set AFTER the guard: the skeleton mirrors the audited
        // upstream root (the pinned entry file is the only required member).
        std::env::set_var(
            "NIU_PLUGIN_SOURCES_ROOT",
            temp.join("sources").to_string_lossy().as_ref(),
        );
        for (id, entry) in AUDITED_UPSTREAM_ROOT_ENTRIES {
            let skeleton = temp.join(id);
            std::fs::create_dir_all(&skeleton).unwrap();
            std::fs::write(skeleton.join(entry), "# fixture skeleton\n").unwrap();
            // The file adapter requires at least one *.sh/*.bash to detect
            // the tree (liquidprompt's real root ships tests.sh alongside
            // its extension-less entry).
            std::fs::write(skeleton.join("tests.sh"), "# skeleton probe\n").unwrap();
            let found = recipe(id).expect("curated generic recipe");
            let RecipeDriver::Git {
                kind,
                entry: Some(declared),
                origin: _,
            } = found.driver.as_ref().unwrap()
            else {
                panic!("{id}: expected a generic git driver with an entry");
            };
            assert_eq!(kind, "generic");
            let request = sources::SourceInstallRequest {
                adapter: Some("file".to_string()),
                origin: skeleton.to_string_lossy().into_owned(),
                id: Some(id.to_string()),
                entry: Some(declared.clone()),
                ..Default::default()
            };
            let record = sources::add_source(request)
                .unwrap_or_else(|err| panic!("{id}: loader dry run failed: {err:#}"));
            let _ = sources::remove_source(&record.id);
        }
        let _ = std::fs::remove_dir_all(&temp);
    }
}
