//! Winuxsh-native plugin inventory, bundle assets, and plugin CLI helpers.
use crate::completion::external::{CommandDef, FlagDef, SubcommandDef};
use crate::config::PluginConfig;
use crate::path_utils::{shell_home_dir, shell_path_to_host_path};
use crate::theme::Theme;
use anyhow::{anyhow, Context};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
pub const OFFICIAL_BUNDLE_NAME: &str = "oh-my-winuxsh";
const OFFICIAL_BUNDLE_VERSION: &str = "1.0.0";
const PLUGIN_API_VERSION: &str = "winuxsh:plugin@0.1.0";
const PLUGIN_BUNDLE_API_VERSION: &str = "winuxsh:plugin-bundle@0.1.0";
const PLUGIN_INDEX_SCHEMA: &str = "winuxsh:plugin-index@0.1.0";
const PLUGIN_INDEX_SIGNATURE_POLICY: &str = "unsupported";
const PROCESS_PLUGIN_PROTOCOL: &str = "winuxsh:process-plugin@0.1.0";
const COMMAND_NOT_FOUND_PROVIDER: &str = "command-not-found";
const SOURCE_PLUGIN_HOOKS: &[&str] = &["startup", "precmd", "preexec", "chpwd"];
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginKind {
    Builtin,
    Source,
    Bridge,
    Process,
}
impl PluginKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Builtin => "builtin",
            Self::Source => "source",
            Self::Bridge => "bridge",
            Self::Process => "process",
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginCategory {
    Devtools,
    Environment,
    Workflow,
    Hints,
    Ux,
}
impl PluginCategory {
    fn as_str(self) -> &'static str {
        match self {
            Self::Devtools => "devtools",
            Self::Environment => "environment",
            Self::Workflow => "workflow",
            Self::Hints => "hints",
            Self::Ux => "ux",
        }
    }
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PluginExportsRecord {
    #[serde(default)]
    pub aliases: bool,
    #[serde(default)]
    pub completions: Vec<String>,
    #[serde(default)]
    pub prompt_segments: Vec<String>,
    #[serde(default)]
    pub hooks: Vec<String>,
    #[serde(default)]
    pub commands: Vec<String>,
    #[serde(default)]
    pub keybindings: Vec<String>,
    #[serde(default)]
    pub themes: Vec<String>,
    #[serde(default)]
    pub providers: Vec<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginProcessSpec {
    pub protocol: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default = "default_timeout_millis")]
    pub timeout_millis: u64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginSourceSpec {
    pub entry: String,
}
fn default_timeout_millis() -> u64 {
    1000
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginPackRecord {
    pub name: String,
    pub bundle: String,
    pub version: String,
    pub kind: PluginKind,
    pub api: String,
    pub category: PluginCategory,
    pub summary: String,
    pub default: bool,
    #[serde(default)]
    pub permissions: Vec<String>,
    #[serde(default)]
    pub required_binaries: Vec<String>,
    #[serde(default)]
    pub exports: PluginExportsRecord,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<PluginSourceSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process: Option<PluginProcessSpec>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginInventory {
    pub bundle: String,
    pub version: String,
    pub api: String,
    pub min_winuxsh: String,
    pub source: String,
    pub trust_source: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<PathBuf>,
    pub packs: Vec<PluginPackRecord>,
}
#[derive(Debug, Clone, Serialize)]
struct PluginInventoryView {
    pub bundle: String,
    pub version: String,
    pub api: String,
    pub min_winuxsh: String,
    pub source: String,
    pub trust_source: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<PathBuf>,
    pub packs: Vec<PluginPackView>,
}
#[derive(Debug, Clone, Serialize)]
struct PluginPackView {
    #[serde(flatten)]
    pub pack: PluginPackRecord,
    pub execution_model: String,
    pub externalization_class: String,
    pub readiness: PluginReadinessProfile,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PluginRuntimeState {
    enabled: BTreeSet<String>,
    decisions: BTreeSet<String>,
}
impl PluginRuntimeState {
    pub fn is_enabled(&self, name: &str) -> bool {
        self.enabled.contains(name)
    }
    pub fn has_decision(&self, name: &str) -> bool {
        self.decisions.contains(name)
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginBundleStatus {
    pub state: String,
    pub bundle: String,
    pub source: String,
    pub trust_source: String,
    pub active_version: Option<String>,
    pub active_api: Option<String>,
    pub min_winuxsh: Option<String>,
    pub active_path: Option<PathBuf>,
    pub bundle_root: PathBuf,
    pub current_path: Option<PathBuf>,
    pub version_path: Option<PathBuf>,
    pub lock_path: PathBuf,
    pub message: String,
    pub candidate_errors: Vec<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginBundleUpdateSummary {
    pub bundle: String,
    pub version: String,
    pub installed_path: PathBuf,
    pub previous_path: Option<PathBuf>,
    pub lock_path: PathBuf,
    pub checksum_sha256: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginBundleRollbackSummary {
    pub bundle: String,
    pub version: String,
    pub active_path: PathBuf,
    pub previous_path: Option<PathBuf>,
    pub lock_path: PathBuf,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginDoctorReport {
    pub ok: bool,
    pub status: String,
    pub enabled: bool,
    pub source: String,
    pub trust_source: String,
    pub packs: Vec<PluginDoctorPack>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginDoctorPack {
    pub name: String,
    pub kind: PluginKind,
    pub execution_model: String,
    pub externalization_class: String,
    pub readiness: PluginReadinessProfile,
    pub status: String,
    pub enabled: bool,
    pub missing_required_binaries: Vec<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginPermissionReview {
    pub plugin: String,
    pub kind: PluginKind,
    pub execution_model: String,
    pub externalization_class: String,
    pub readiness: PluginReadinessProfile,
    pub trust_source: String,
    pub currently_enabled: bool,
    pub permissions: Vec<PermissionReviewItem>,
    pub missing_required_binaries: Vec<String>,
    pub install_command: String,
    pub notes: Vec<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionReviewItem {
    pub token: String,
    pub risk: String,
    pub scope: String,
    pub description: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
struct PluginSearchResult {
    pack: PluginPackRecord,
    execution_model: String,
    externalization_class: String,
    readiness: PluginReadinessProfile,
    matched_fields: Vec<String>,
    source: String,
    trust_source: String,
}
#[derive(Debug, Clone, Serialize)]
struct PluginPackInfo {
    #[serde(flatten)]
    pack: PluginPackRecord,
    execution_model: String,
    externalization_class: String,
    readiness: PluginReadinessProfile,
    source: String,
    trust_source: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginReadinessProfile {
    pub target_runtime: String,
    pub missing_host_api_or_decision: String,
    pub shell_mutating: bool,
    pub fallback_needed: bool,
    pub fallback: String,
}
#[derive(Debug, Clone, Serialize)]
pub struct PluginThemeCatalogEntry {
    pub name: String,
    pub source: String,
    pub trust_source: String,
    pub owner: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bundle: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pack: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<PathBuf>,
}
#[derive(Debug, Deserialize)]
struct BundleToml {
    name: String,
    version: String,
    api: String,
    min_winuxsh: String,
    packs: BundlePacksToml,
    #[serde(default)]
    layout: BundleLayoutToml,
}
#[derive(Debug, Deserialize)]
struct BundlePacksToml {
    #[serde(default)]
    default: Vec<String>,
    #[serde(default)]
    available: Vec<String>,
}
#[derive(Debug, Clone, Deserialize)]
struct BundleLayoutToml {
    #[serde(default = "default_packs_dir")]
    packs_dir: String,
    #[serde(default = "default_aliases_dir")]
    aliases_dir: String,
    #[serde(default = "default_completions_dir")]
    completions_dir: String,
    #[serde(default = "default_prompts_dir")]
    prompts_dir: String,
    #[serde(default = "default_keybindings_dir")]
    keybindings_dir: String,
    #[serde(default = "default_themes_dir")]
    themes_dir: String,
}
impl Default for BundleLayoutToml {
    fn default() -> Self {
        Self {
            packs_dir: default_packs_dir(),
            aliases_dir: default_aliases_dir(),
            completions_dir: default_completions_dir(),
            prompts_dir: default_prompts_dir(),
            keybindings_dir: default_keybindings_dir(),
            themes_dir: default_themes_dir(),
        }
    }
}
fn default_packs_dir() -> String {
    "packs".to_string()
}
fn default_aliases_dir() -> String {
    "aliases".to_string()
}
fn default_completions_dir() -> String {
    "completions".to_string()
}
fn default_prompts_dir() -> String {
    "prompts".to_string()
}
fn default_keybindings_dir() -> String {
    "keybindings".to_string()
}
fn default_themes_dir() -> String {
    "themes".to_string()
}
#[derive(Debug, Deserialize)]
struct BundlePackToml {
    name: String,
    bundle: String,
    version: String,
    kind: PluginKind,
    api: String,
    category: PluginCategory,
    summary: String,
    default: bool,
    #[serde(default)]
    permissions: Vec<String>,
    #[serde(default)]
    required_binaries: Vec<String>,
    #[serde(default)]
    exports: PluginExportsRecord,
    #[serde(default)]
    source: Option<PluginSourceSpec>,
    #[serde(default)]
    process: Option<PluginProcessSpec>,
}
#[derive(Debug, Deserialize)]
struct FrameworkPluginToml {
    name: String,
    version: String,
    kind: PluginKind,
    entry: String,
    summary: String,
    #[serde(default)]
    api: Option<String>,
    #[serde(default)]
    bundle: Option<String>,
    #[serde(default)]
    category: Option<PluginCategory>,
    #[serde(default)]
    default: Option<bool>,
    #[serde(default)]
    permissions: Vec<String>,
    #[serde(default)]
    required_binaries: Vec<String>,
    #[serde(default)]
    exports: PluginExportsRecord,
}
#[derive(Debug, Deserialize)]
struct BundleIndexToml {
    schema: String,
    bundle: String,
    version: String,
    bundle_api: String,
    min_winuxsh: String,
    release: BundleIndexReleaseToml,
    packs: Vec<BundleIndexPackToml>,
}
#[derive(Debug, Deserialize)]
struct BundleIndexReleaseToml {
    artifact: String,
    checksum: String,
    checksum_algorithm: String,
    checksum_required: bool,
    signature: String,
}
#[derive(Debug, Deserialize)]
struct BundleIndexPackToml {
    name: String,
    version: String,
    api: String,
    kind: PluginKind,
    category: PluginCategory,
    summary: String,
    default: bool,
    #[serde(default)]
    permissions: Vec<String>,
    #[serde(default)]
    required_binaries: Vec<String>,
}
#[derive(Debug, Serialize, Deserialize)]
struct PluginLockToml {
    bundle: String,
    version: String,
    active_path: PathBuf,
    previous_path: Option<PathBuf>,
    checksum_sha256: Option<String>,
}
#[derive(Debug, Deserialize)]
struct BundleAliasesToml {
    #[serde(default)]
    aliases: BTreeMap<String, String>,
}
#[derive(Debug, Deserialize)]
struct BundlePromptSegmentsToml {
    #[serde(default)]
    segments: BTreeMap<String, BundlePromptSegmentToml>,
    #[serde(default)]
    presets: BTreeMap<String, BundlePromptPresetToml>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptPresetAsset {
    #[serde(default)]
    pub left_elements: Vec<String>,
    #[serde(default)]
    pub right_elements: Vec<String>,
    #[serde(default)]
    pub separator: String,
    #[serde(default)]
    pub git_prompt_format: Option<String>,
}
#[derive(Debug, Deserialize)]
struct BundlePromptPresetToml {
    #[serde(default, alias = "left_elements")]
    left: Vec<String>,
    #[serde(default, alias = "right_elements")]
    right: Vec<String>,
    #[serde(default)]
    separator: String,
    #[serde(default)]
    git_prompt_format: Option<String>,
}
#[allow(dead_code)]
#[derive(Debug, Deserialize)]
struct BundlePromptSegmentToml {
    id: String,
    #[serde(default)]
    description: Option<String>,
}
impl From<BundlePromptPresetToml> for PromptPresetAsset {
    fn from(value: BundlePromptPresetToml) -> Self {
        Self {
            left_elements: value.left,
            right_elements: value.right,
            separator: value.separator,
            git_prompt_format: value.git_prompt_format,
        }
    }
}
#[derive(Debug, Deserialize)]
struct BundleKeybindingsToml {
    name: String,
    summary: String,
    keymap: String,
    #[serde(default)]
    bindings: Vec<BundleKeybindingToml>,
}
#[allow(dead_code)]
#[derive(Debug, Deserialize)]
struct BundleKeybindingToml {
    key: String,
    action: String,
}
pub fn effective_plugin_state(config: &PluginConfig) -> PluginRuntimeState {
    let mut state = PluginRuntimeState::default();
    if !config.enabled
        || !config
            .bundles
            .iter()
            .any(|bundle| bundle == OFFICIAL_BUNDLE_NAME)
    {
        return state;
    }
    for pack in active_plugin_inventory().packs {
        if pack.default {
            state.enabled.insert(pack.name);
        }
    }
    for name in &config.load {
        state.enabled.insert(name.clone());
        state.decisions.insert(name.clone());
    }
    for (name, pack) in &config.packs {
        if let Some(enabled) = pack.enabled {
            state.decisions.insert(name.clone());
            if enabled {
                state.enabled.insert(name.clone());
            } else {
                state.enabled.remove(name);
            }
        }
    }
    state
}
pub fn active_plugin_inventory() -> PluginInventory {
    active_plugin_inventory_result().unwrap_or_else(|err| {
        log::warn!("falling back to compiled plugin inventory: {}", err);
        compiled_plugin_inventory()
    })
}
fn active_plugin_inventory_result() -> anyhow::Result<PluginInventory> {
    if let Some(path) = env_path("WINUXSH_PLUGIN_BUNDLE_PATH") {
        if path.exists() {
            return load_official_bundle_inventory_from_path(&path, "env_override");
        }
    }
    let lock_path = plugin_lock_path();
    if let Ok(lock) = read_plugin_lock(&lock_path) {
        if lock.active_path.exists() {
            return load_official_bundle_inventory_from_path(&lock.active_path, "user_bundle");
        }
    }
    for path in app_bundled_bundle_candidates() {
        if path.exists() {
            return load_official_bundle_inventory_from_path(&path, "app_bundle");
        }
    }
    Ok(compiled_plugin_inventory())
}
fn load_official_bundle_inventory_from_path(
    path: &Path,
    source: &str,
) -> anyhow::Result<PluginInventory> {
    let bundle_path = path.join("bundle.toml");
    let bundle_text = fs::read_to_string(&bundle_path)
        .with_context(|| format!("failed to read {}", bundle_path.display()))?;
    let bundle: BundleToml = toml::from_str(&bundle_text)
        .with_context(|| format!("failed to parse {}", bundle_path.display()))?;
    let mut packs = load_legacy_bundle_pack_records(path, &bundle)?;
    let framework_packs = load_framework_plugin_records(path, &bundle)?;
    if !framework_packs.is_empty() {
        let framework_names = framework_packs
            .iter()
            .map(|pack| pack.name.clone())
            .collect::<BTreeSet<_>>();
        let has_prompt_core = framework_names.contains("prompt-core");
        let has_theme_plugins = framework_names
            .iter()
            .any(|name| name.starts_with("theme-"));
        packs.retain(|pack| {
            !framework_names.contains(&pack.name)
                && !(pack.name == "prompts" && has_prompt_core)
                && !(pack.name == "themes" && has_theme_plugins)
        });
        packs.extend(framework_packs);
    }
    let trust_source = plugin_trust_source_for(&bundle.name, source).to_string();
    Ok(PluginInventory {
        bundle: bundle.name,
        version: bundle.version,
        api: bundle.api,
        min_winuxsh: bundle.min_winuxsh,
        source: source.to_string(),
        trust_source,
        path: Some(path.to_path_buf()),
        packs,
    })
}
fn load_legacy_bundle_pack_records(
    path: &Path,
    bundle: &BundleToml,
) -> anyhow::Result<Vec<PluginPackRecord>> {
    let pack_names = if bundle.packs.available.is_empty() {
        bundle.packs.default.clone()
    } else {
        bundle.packs.available.clone()
    };
    let mut packs = Vec::new();
    for name in pack_names {
        let manifest_path = path
            .join(&bundle.layout.packs_dir)
            .join(&name)
            .join("plugin.toml");
        if !manifest_path.is_file() {
            log::warn!(
                "skipping missing optional bundle pack '{}' at {}",
                name,
                manifest_path.display()
            );
            continue;
        }
        let text = fs::read_to_string(&manifest_path)
            .with_context(|| format!("failed to read {}", manifest_path.display()))?;
        let manifest: BundlePackToml = toml::from_str(&text)
            .with_context(|| format!("failed to parse {}", manifest_path.display()))?;
        packs.push(PluginPackRecord {
            name: manifest.name,
            bundle: manifest.bundle,
            version: manifest.version,
            kind: manifest.kind,
            api: manifest.api,
            category: manifest.category,
            summary: manifest.summary,
            default: manifest.default,
            permissions: manifest.permissions,
            required_binaries: manifest.required_binaries,
            exports: manifest.exports,
            source: manifest.source,
            process: manifest.process,
        });
    }
    Ok(packs)
}
fn load_framework_plugin_records(
    path: &Path,
    bundle: &BundleToml,
) -> anyhow::Result<Vec<PluginPackRecord>> {
    let plugins_dir = path.join("plugins");
    if !plugins_dir.is_dir() {
        return Ok(Vec::new());
    }

    let mut manifests = Vec::new();
    for entry in fs::read_dir(&plugins_dir)
        .with_context(|| format!("failed to read {}", plugins_dir.display()))?
    {
        let entry = entry?;
        let plugin_dir = entry.path();
        if !plugin_dir.is_dir() {
            continue;
        }
        let Some(name) = plugin_dir.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let manifest_path = plugin_dir.join("plugin.toml");
        if manifest_path.is_file() {
            manifests.push((name.to_string(), manifest_path));
        }
    }
    manifests.sort_by(|(left, _), (right, _)| {
        framework_plugin_sort_key(left)
            .cmp(&framework_plugin_sort_key(right))
            .then_with(|| left.cmp(right))
    });

    let mut packs = Vec::new();
    for (dir_name, manifest_path) in manifests {
        let text = fs::read_to_string(&manifest_path)
            .with_context(|| format!("failed to read {}", manifest_path.display()))?;
        let manifest: FrameworkPluginToml = toml::from_str(&text)
            .with_context(|| format!("failed to parse {}", manifest_path.display()))?;
        if manifest.name != dir_name {
            anyhow::bail!(
                "framework plugin manifest {} names '{}' but lives in '{}'",
                manifest_path.display(),
                manifest.name,
                dir_name
            );
        }
        if !safe_asset_name(&manifest.name) {
            anyhow::bail!("framework plugin '{}' has an unsafe name", manifest.name);
        }
        let source = if manifest.kind == PluginKind::Source {
            Some(PluginSourceSpec {
                entry: framework_plugin_source_entry(&dir_name, &manifest.entry)?,
            })
        } else {
            None
        };
        packs.push(PluginPackRecord {
            name: manifest.name.clone(),
            bundle: manifest.bundle.unwrap_or_else(|| bundle.name.clone()),
            version: manifest.version,
            kind: manifest.kind,
            api: manifest
                .api
                .unwrap_or_else(|| PLUGIN_API_VERSION.to_string()),
            category: manifest
                .category
                .unwrap_or_else(|| framework_plugin_category(&manifest.name, &manifest.exports)),
            summary: manifest.summary,
            default: manifest
                .default
                .unwrap_or_else(|| framework_plugin_default(&manifest.name)),
            permissions: manifest.permissions,
            required_binaries: manifest.required_binaries,
            exports: manifest.exports,
            source,
            process: None,
        });
    }
    Ok(packs)
}
fn framework_plugin_source_entry(dir_name: &str, entry: &str) -> anyhow::Result<String> {
    let path = Path::new(entry);
    if entry.is_empty()
        || path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        anyhow::bail!(
            "framework plugin '{}' entry '{}' must be plugin-local relative path",
            dir_name,
            entry
        );
    }
    Ok(format!("plugins/{}/{}", dir_name, entry.replace('\\', "/")))
}
fn framework_plugin_category(name: &str, exports: &PluginExportsRecord) -> PluginCategory {
    if name == COMMAND_NOT_FOUND_PROVIDER
        || exports
            .providers
            .iter()
            .any(|provider| provider == COMMAND_NOT_FOUND_PROVIDER)
    {
        return PluginCategory::Hints;
    }
    if matches!(name, "direnv" | "dotenv") {
        return PluginCategory::Environment;
    }
    if matches!(name, "git" | "docker" | "kubectl" | "npm") {
        return PluginCategory::Devtools;
    }
    if name == "prompt-core"
        || name == "keybindings"
        || name.starts_with("theme-")
        || !exports.themes.is_empty()
        || !exports.keybindings.is_empty()
        || !exports.prompt_segments.is_empty()
    {
        return PluginCategory::Ux;
    }
    PluginCategory::Workflow
}
fn framework_plugin_default(name: &str) -> bool {
    matches!(
        name,
        "prompt-core" | "git" | "theme-minimal" | "keybindings"
    )
}
fn framework_plugin_sort_key(name: &str) -> u8 {
    match name {
        "prompt-core" => 0,
        "git" => 10,
        "docker" | "kubectl" | "npm" => 20,
        "zoxide" | "direnv" | "dotenv" | "fzf" | "last-working-dir" | "thefuck" => 30,
        "command-not-found" | "keybindings" => 40,
        "theme-minimal" => 50,
        _ if name.starts_with("theme-") => 60,
        _ => 100,
    }
}
fn compiled_plugin_inventory() -> PluginInventory {
    PluginInventory {
        bundle: OFFICIAL_BUNDLE_NAME.to_string(),
        version: OFFICIAL_BUNDLE_VERSION.to_string(),
        api: PLUGIN_BUNDLE_API_VERSION.to_string(),
        min_winuxsh: env!("CARGO_PKG_VERSION").to_string(),
        source: "compiled_fallback".to_string(),
        trust_source: "official_bundle".to_string(),
        path: None,
        packs: compiled_plugin_packs(),
    }
}
fn compiled_plugin_packs() -> Vec<PluginPackRecord> {
    vec![
        static_pack(
            "git",
            PluginKind::Builtin,
            PluginCategory::Devtools,
            "Git aliases, completions, and prompt segments.",
            true,
            &["cwd:read", "process:run:git"],
            &["git"],
            exports(true, &["git"], &["git"], &[], &[], &[], &[]),
        ),
        static_pack(
            "docker",
            PluginKind::Builtin,
            PluginCategory::Devtools,
            "Docker aliases and completion hints.",
            false,
            &["cwd:read", "process:run:docker"],
            &["docker"],
            exports(true, &["docker"], &[], &[], &[], &[], &[]),
        ),
        static_pack(
            "kubectl",
            PluginKind::Builtin,
            PluginCategory::Devtools,
            "Kubectl aliases and completion hints.",
            false,
            &["cwd:read", "process:run:kubectl"],
            &["kubectl"],
            exports(true, &["kubectl"], &[], &[], &[], &[], &[]),
        ),
        static_pack(
            "npm",
            PluginKind::Builtin,
            PluginCategory::Devtools,
            "npm aliases and runtime completion shape detection.",
            false,
            &["cwd:read", "process:run:npm"],
            &["npm"],
            exports(true, &["npm"], &[], &[], &[], &[], &[]),
        ),
        static_pack(
            "zoxide",
            PluginKind::Builtin,
            PluginCategory::Workflow,
            "z command shim plus directory tracking.",
            false,
            &["cwd:read", "shell:cwd:write", "process:run:zoxide"],
            &["zoxide"],
            exports(false, &[], &[], &[], &["z"], &[], &[]),
        ),
        static_pack(
            "direnv",
            PluginKind::Builtin,
            PluginCategory::Environment,
            "direnv environment export on Winuxsh lifecycle hooks.",
            false,
            &["cwd:read", "env:write", "process:run:direnv"],
            &["direnv"],
            exports(false, &[], &[], &["chpwd", "precmd"], &[], &[], &[]),
        ),
        static_pack(
            "dotenv",
            PluginKind::Builtin,
            PluginCategory::Environment,
            "Safe .env loader on Winuxsh lifecycle hooks.",
            false,
            &["cwd:read", "fs:read:.env", "env:write"],
            &[],
            exports(false, &[], &[], &["chpwd", "precmd"], &[], &[], &[]),
        ),
        static_pack(
            "fzf",
            PluginKind::Builtin,
            PluginCategory::Workflow,
            "Interactive directory selector commands.",
            false,
            &["cwd:read", "shell:cwd:write", "process:run:fzf"],
            &["fzf"],
            exports(false, &[], &[], &[], &["cdf", "fzf-cd"], &[], &[]),
        ),
        static_pack(
            "command-not-found",
            PluginKind::Builtin,
            PluginCategory::Hints,
            "Interactive missing-command hints for native Windows.",
            false,
            &["command:diagnose"],
            &[],
            exports_with_providers(false, &[], &[], &[], &[], &[], &[], &["command-not-found"]),
        ),
        static_pack(
            "last-working-dir",
            PluginKind::Builtin,
            PluginCategory::Workflow,
            "Last working directory cache and restore command.",
            false,
            &["cwd:read", "fs:read:cache", "fs:write:cache"],
            &[],
            exports(false, &[], &[], &["startup", "chpwd"], &[], &[], &[]),
        ),
        static_pack(
            "thefuck",
            PluginKind::Builtin,
            PluginCategory::Workflow,
            "Correction shim for the previous interactive command.",
            false,
            &[
                "history:read",
                "shell:execute:suggested-command",
                "process:run:thefuck",
            ],
            &["thefuck"],
            exports(false, &[], &[], &[], &["fuck"], &[], &[]),
        ),
        static_pack(
            "keybindings",
            PluginKind::Builtin,
            PluginCategory::Ux,
            "Winuxsh keybinding presets mapped to native reedline actions.",
            true,
            &[],
            &[],
            exports(false, &[], &[], &[], &[], &["common", "emacs", "vi"], &[]),
        ),
        static_pack(
            "prompts",
            PluginKind::Builtin,
            PluginCategory::Ux,
            "Prompt presets and reusable prompt segments.",
            true,
            &[],
            &[],
            exports(
                false,
                &[],
                &["cwd", "git", "status", "time", "user_host"],
                &[],
                &[],
                &[],
                &[],
            ),
        ),
        static_pack(
            "themes",
            PluginKind::Builtin,
            PluginCategory::Ux,
            "Official Winuxsh color themes for prompt and Git status styling.",
            true,
            &[],
            &[],
            exports(false, &[], &[], &[], &[], &[], &[]),
        ),
    ]
}
fn static_pack(
    name: &str,
    kind: PluginKind,
    category: PluginCategory,
    summary: &str,
    default: bool,
    permissions: &[&str],
    required_binaries: &[&str],
    exports: PluginExportsRecord,
) -> PluginPackRecord {
    PluginPackRecord {
        name: name.to_string(),
        bundle: OFFICIAL_BUNDLE_NAME.to_string(),
        version: OFFICIAL_BUNDLE_VERSION.to_string(),
        kind,
        api: PLUGIN_API_VERSION.to_string(),
        category,
        summary: summary.to_string(),
        default,
        permissions: strings_from_static(permissions),
        required_binaries: strings_from_static(required_binaries),
        exports,
        source: None,
        process: None,
    }
}
fn exports(
    aliases: bool,
    completions: &[&str],
    prompt_segments: &[&str],
    hooks: &[&str],
    commands: &[&str],
    keybindings: &[&str],
    themes: &[&str],
) -> PluginExportsRecord {
    PluginExportsRecord {
        aliases,
        completions: strings_from_static(completions),
        prompt_segments: strings_from_static(prompt_segments),
        hooks: strings_from_static(hooks),
        commands: strings_from_static(commands),
        keybindings: strings_from_static(keybindings),
        themes: strings_from_static(themes),
        providers: Vec::new(),
    }
}
fn exports_with_providers(
    aliases: bool,
    completions: &[&str],
    prompt_segments: &[&str],
    hooks: &[&str],
    commands: &[&str],
    keybindings: &[&str],
    themes: &[&str],
    providers: &[&str],
) -> PluginExportsRecord {
    let mut exports = exports(
        aliases,
        completions,
        prompt_segments,
        hooks,
        commands,
        keybindings,
        themes,
    );
    exports.providers = strings_from_static(providers);
    exports
}
fn strings_from_static(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_string()).collect()
}
fn plugin_inventory_view(inventory: PluginInventory) -> PluginInventoryView {
    PluginInventoryView {
        bundle: inventory.bundle,
        version: inventory.version,
        api: inventory.api,
        min_winuxsh: inventory.min_winuxsh,
        source: inventory.source,
        trust_source: inventory.trust_source,
        path: inventory.path,
        packs: inventory.packs.into_iter().map(plugin_pack_view).collect(),
    }
}
fn plugin_pack_view(pack: PluginPackRecord) -> PluginPackView {
    PluginPackView {
        execution_model: plugin_execution_model(&pack).to_string(),
        externalization_class: plugin_externalization_class(&pack).to_string(),
        readiness: plugin_readiness_profile(&pack),
        pack,
    }
}
fn plugin_execution_model(pack: &PluginPackRecord) -> &'static str {
    match pack.kind {
        PluginKind::Builtin if plugin_externalization_class(pack) == "declarative_asset" => {
            "none_declarative"
        }
        PluginKind::Builtin => "host_builtin",
        PluginKind::Source => "shell_source",
        PluginKind::Bridge => "host_bridge",
        PluginKind::Process if !pack.exports.providers.is_empty() => "process_provider",
        PluginKind::Process => "process",
    }
}
fn plugin_externalization_class(pack: &PluginPackRecord) -> &'static str {
    if pack
        .exports
        .providers
        .iter()
        .any(|provider| provider == COMMAND_NOT_FOUND_PROVIDER)
    {
        return "pure_provider_candidate";
    }
    if pack.kind == PluginKind::Source {
        return "shell_source";
    }
    match pack.name.as_str() {
        "docker" | "kubectl" | "keybindings" | "themes" => "declarative_asset",
        "git" | "npm" | "prompts" => "mixed_declarative_native",
        "command-not-found" => "pure_provider_candidate",
        "zoxide" | "direnv" | "dotenv" | "fzf" | "last-working-dir" | "thefuck" => {
            "shell_effect_candidate"
        }
        "process-echo" | "process-hook" => "fixture",
        _ => match pack.kind {
            PluginKind::Builtin if builtin_pack_is_asset_only(pack) => "declarative_asset",
            PluginKind::Builtin => "host_builtin",
            PluginKind::Source => "shell_source",
            PluginKind::Bridge => "host_bridge",
            PluginKind::Process => "external_tool_adapter",
        },
    }
}
fn builtin_pack_is_asset_only(pack: &PluginPackRecord) -> bool {
    pack.kind == PluginKind::Builtin
        && pack.exports.hooks.is_empty()
        && pack.exports.commands.is_empty()
        && pack.exports.providers.is_empty()
        && pack.required_binaries.is_empty()
        && pack.permissions.is_empty()
}
fn readiness(
    target_runtime: &str,
    missing_host_api_or_decision: &str,
    shell_mutating: bool,
    fallback_needed: bool,
    fallback: &str,
) -> PluginReadinessProfile {
    PluginReadinessProfile {
        target_runtime: target_runtime.to_string(),
        missing_host_api_or_decision: missing_host_api_or_decision.to_string(),
        shell_mutating,
        fallback_needed,
        fallback: fallback.to_string(),
    }
}
fn plugin_readiness_profile(pack: &PluginPackRecord) -> PluginReadinessProfile {
    if pack.kind == PluginKind::Source
        && matches!(
            pack.name.as_str(),
            "zoxide" | "direnv" | "dotenv" | "fzf" | "last-working-dir" | "thefuck"
        )
    {
        return readiness("current_shell_source", "none", true, false, "none");
    }

    match pack.name.as_str() {
        "git" => readiness(
            "source_assets_plus_native_prompt_segment",
            "prompt_segment_provider_abi",
            false,
            true,
            "compiled_prompt_segment",
        ),
        "docker" => readiness(
            "current_shell_source_plus_declarative_assets",
            "none",
            false,
            true,
            "minimal_compiled_alias_completion_fallback",
        ),
        "kubectl" => readiness(
            "current_shell_source_plus_declarative_assets",
            "none",
            false,
            true,
            "minimal_compiled_alias_completion_fallback",
        ),
        "npm" => readiness(
            "source_assets_plus_native_dynamic_completion",
            "completion_provider_abi",
            false,
            true,
            "native_dynamic_completion",
        ),
        "zoxide" => readiness(
            "native_until_effect_runtime",
            "shell:cwd:write,lifecycle_context,rollback_behavior",
            true,
            true,
            "native_builtin",
        ),
        "direnv" => readiness(
            "native_until_effect_runtime",
            "env:write,lifecycle_context,rollback_behavior",
            true,
            true,
            "native_builtin",
        ),
        "dotenv" => readiness(
            "native_until_effect_runtime",
            "scoped_fs_read,env:write,lifecycle_context,rollback_behavior",
            true,
            true,
            "native_builtin",
        ),
        "fzf" => readiness(
            "native_until_interactive_effect_runtime",
            "interactive_process_policy,shell:cwd:write,rollback_behavior",
            true,
            true,
            "native_builtin",
        ),
        "command-not-found" => readiness(
            "process_provider_available_builtin_until_migration",
            "bundle_migration_decision,provider_abi",
            false,
            true,
            "compiled_native_hints",
        ),
        "last-working-dir" => readiness(
            "native_until_effect_runtime",
            "cache_read_write,startup_chpwd_effect_protocol,shell:cwd:write",
            true,
            true,
            "native_builtin",
        ),
        "thefuck" => readiness(
            "native_or_process_adapter_after_effect_protocol",
            "history:read,suggested_command_review_execute_protocol,process_adapter_policy",
            true,
            true,
            "native_builtin",
        ),
        "keybindings" => readiness(
            "asset_only_declarative_schema_tbd",
            "asset_only_schema_marker,reedline_actions_stay_native",
            false,
            true,
            "native_reedline_actions",
        ),
        "prompts" => readiness(
            "declarative_presets_plus_native_segments",
            "prompt_segment_provider_abi",
            false,
            true,
            "native_prompt_segments",
        ),
        "themes" => readiness(
            "asset_only_declarative_schema_tbd",
            "asset_only_schema_marker,theme_renderer_stays_native",
            false,
            true,
            "native_theme_renderer",
        ),
        "process-echo" => readiness("process_command_fixture", "none", false, false, "none"),
        "process-hook" => readiness(
            "process_hook_fixture_not_effect_runtime",
            "structured_hook_effects_before_shell_mutating_use",
            false,
            false,
            "none",
        ),
        _ => match plugin_externalization_class(pack) {
            "declarative_asset" => readiness(
                "asset_only_declarative_schema_tbd",
                "asset_only_schema_marker",
                false,
                true,
                "compiled_or_bundle_asset_fallback",
            ),
            "shell_effect_candidate" => readiness(
                "native_until_effect_runtime",
                "effect_protocol",
                true,
                true,
                "native_builtin",
            ),
            "pure_provider_candidate" => readiness(
                "provider_runtime_tbd",
                "provider_abi",
                false,
                true,
                "compiled_native_fallback",
            ),
            "fixture" => readiness("fixture", "none", false, false, "none"),
            "shell_source" => readiness("current_shell_source", "none", true, false, "none"),
            _ => match pack.kind {
                PluginKind::Builtin => readiness(
                    "host_builtin_until_classified",
                    "readiness_classification",
                    false,
                    true,
                    "host_builtin",
                ),
                PluginKind::Source => {
                    readiness("current_shell_source", "none", true, false, "none")
                }
                PluginKind::Bridge => readiness(
                    "host_bridge",
                    "host_api_boundary",
                    false,
                    true,
                    "native_host_bridge",
                ),
                PluginKind::Process => readiness("process_adapter", "none", false, false, "none"),
            },
        },
    }
}
fn push_readiness_text(out: &mut String, readiness: &PluginReadinessProfile) {
    out.push_str(&format!("Target runtime: {}\n", readiness.target_runtime));
    out.push_str(&format!(
        "Missing host API/decision: {}\n",
        readiness.missing_host_api_or_decision
    ));
    out.push_str(&format!(
        "Shell-mutating: {}\n",
        if readiness.shell_mutating {
            "yes"
        } else {
            "no"
        }
    ));
    out.push_str(&format!(
        "Fallback needed: {}\n",
        if readiness.fallback_needed {
            "yes"
        } else {
            "no"
        }
    ));
    out.push_str(&format!("Fallback: {}\n", readiness.fallback));
}
pub fn plugin_packs_json() -> anyhow::Result<String> {
    Ok(serde_json::to_string_pretty(&plugin_inventory_view(
        active_plugin_inventory(),
    ))?)
}
pub fn plugin_packs_text() -> String {
    let inventory = active_plugin_inventory();
    let mut out = String::new();
    if inventory.trust_source == "official_bundle" {
        out.push_str("Official Winuxsh plugins\n");
    } else {
        out.push_str("Winuxsh plugin inventory\n");
    }
    out.push_str(&format!("Bundle: {}\n", inventory.bundle));
    out.push_str(&format!("Source: {}\n", inventory.source));
    out.push_str(&format!("Trust source: {}\n", inventory.trust_source));
    match inventory.trust_source.as_str() {
        "external_bundle" => out.push_str(
            "External bundle packs are review-only until registry trust policy is implemented.\n",
        ),
        "local_override" => {
            out.push_str("Local override bundle; review packs before managed install.\n")
        }
        _ => out.push_str("These are Winuxsh-native packs managed through [plugins].\n"),
    }
    for pack in &inventory.packs {
        out.push_str(&pack_list_line(pack));
    }
    out
}
fn pack_list_line(pack: &PluginPackRecord) -> String {
    let readiness = plugin_readiness_profile(pack);
    format!(
        "- {} kind={} category={} default={} execution={} externalization={} target={} shell_mutating={} fallback={}\n",
        pack.name,
        pack.kind.as_str(),
        pack.category.as_str(),
        if pack.default { "on" } else { "off" },
        plugin_execution_model(pack),
        plugin_externalization_class(pack),
        readiness.target_runtime,
        if readiness.shell_mutating { "yes" } else { "no" },
        if readiness.fallback_needed { "yes" } else { "no" }
    )
}
pub fn plugin_pack_json(name: &str) -> anyhow::Result<Option<String>> {
    let inventory = active_plugin_inventory();
    let source = inventory.source.clone();
    let trust_source = inventory.trust_source.clone();
    let Some(pack) = inventory
        .packs
        .into_iter()
        .find(|pack| pack.name.eq_ignore_ascii_case(name))
    else {
        return Ok(None);
    };
    Ok(Some(serde_json::to_string_pretty(&PluginPackInfo {
        execution_model: plugin_execution_model(&pack).to_string(),
        externalization_class: plugin_externalization_class(&pack).to_string(),
        readiness: plugin_readiness_profile(&pack),
        pack,
        source,
        trust_source,
    })?))
}
pub fn plugin_pack_text(name: &str) -> Option<String> {
    let inventory = active_plugin_inventory();
    let pack = inventory
        .packs
        .iter()
        .find(|pack| pack.name.eq_ignore_ascii_case(name))?;
    let mut out = String::new();
    out.push_str(&format!("Plugin: {}\n", pack.name));
    out.push_str(&format!("Bundle: {}\n", pack.bundle));
    out.push_str(&format!("Source: {}\n", inventory.source));
    out.push_str(&format!("Trust source: {}\n", inventory.trust_source));
    out.push_str(&format!("Version: {}\n", pack.version));
    out.push_str(&format!("Kind: {}\n", pack.kind.as_str()));
    out.push_str(&format!(
        "Execution model: {}\n",
        plugin_execution_model(pack)
    ));
    out.push_str(&format!(
        "Externalization class: {}\n",
        plugin_externalization_class(pack)
    ));
    let readiness = plugin_readiness_profile(pack);
    push_readiness_text(&mut out, &readiness);
    out.push_str(&format!("Category: {}\n", pack.category.as_str()));
    out.push_str(&format!(
        "Default: {}\n",
        if pack.default { "on" } else { "off" }
    ));
    out.push_str(&format!("Summary: {}\n", pack.summary));
    out.push_str(&format!(
        "Permissions: {}\n",
        list_or_none(&pack.permissions)
    ));
    out.push_str(&format!(
        "Required binaries: {}\n",
        list_or_none(&pack.required_binaries)
    ));
    out.push_str("Exports:\n");
    out.push_str(&format!(
        "  aliases: {}\n",
        if pack.exports.aliases { "yes" } else { "no" }
    ));
    out.push_str(&format!(
        "  completions: {}\n",
        list_or_none(&pack.exports.completions)
    ));
    out.push_str(&format!(
        "  prompt_segments: {}\n",
        list_or_none(&pack.exports.prompt_segments)
    ));
    out.push_str(&format!("  hooks: {}\n", list_or_none(&pack.exports.hooks)));
    out.push_str(&format!(
        "  commands: {}\n",
        list_or_none(&pack.exports.commands)
    ));
    out.push_str(&format!(
        "  keybindings: {}\n",
        list_or_none(&pack.exports.keybindings)
    ));
    out.push_str(&format!(
        "  themes: {}\n",
        list_or_none(&pack.exports.themes)
    ));
    out.push_str(&format!(
        "  providers: {}\n",
        list_or_none(&pack.exports.providers)
    ));
    if let Some(source) = &pack.source {
        out.push_str("Source:\n");
        out.push_str(&format!("  entry: {}\n", source.entry));
    }
    let keybinding_lines = plugin_keybinding_metadata_lines(&inventory, pack);
    if !keybinding_lines.is_empty() {
        out.push_str("Keybinding metadata:\n");
        for line in keybinding_lines {
            out.push_str("  ");
            out.push_str(&line);
            out.push('\n');
        }
    }
    if let Some(process) = &pack.process {
        out.push_str("Process:\n");
        out.push_str(&format!("  protocol: {}\n", process.protocol));
        out.push_str(&format!("  command: {}\n", process.command));
        out.push_str(&format!("  args: {}\n", list_or_none(&process.args)));
        out.push_str(&format!("  timeout_millis: {}\n", process.timeout_millis));
        out.push_str(&format!(
            "  commands: {}\n",
            list_or_none(&pack.exports.commands)
        ));
    }
    Some(out)
}
fn list_or_none(values: &[String]) -> String {
    if values.is_empty() {
        "none".to_string()
    } else {
        values.join(",")
    }
}
pub fn plugin_search_json(query: Option<&str>) -> anyhow::Result<String> {
    Ok(serde_json::to_string_pretty(&plugin_search_results(query))?)
}
pub fn plugin_search_text(query: Option<&str>) -> String {
    let inventory = active_plugin_inventory();
    let mut out = String::new();
    out.push_str("Plugin search\n");
    out.push_str(&format!("Source: {}\n", inventory.source));
    out.push_str(&format!("Trust source: {}\n", inventory.trust_source));
    if let Some(query) = query {
        out.push_str(&format!("Query: {}\n", query));
    }
    for result in plugin_search_results_from_inventory(query, inventory) {
        out.push_str(&pack_list_line(&result.pack));
    }
    out
}
fn plugin_search_results(query: Option<&str>) -> Vec<PluginSearchResult> {
    let inventory = active_plugin_inventory();
    plugin_search_results_from_inventory(query, inventory)
}
fn plugin_search_results_from_inventory(
    query: Option<&str>,
    inventory: PluginInventory,
) -> Vec<PluginSearchResult> {
    let needle = query.map(|query| query.to_ascii_lowercase());
    let source = inventory.source;
    let trust_source = inventory.trust_source;
    inventory
        .packs
        .into_iter()
        .filter_map(|pack| {
            let mut matched_fields = Vec::new();
            if let Some(needle) = &needle {
                if pack.name.to_ascii_lowercase().contains(needle) {
                    matched_fields.push("name".to_string());
                }
                if pack.category.as_str().contains(needle) {
                    matched_fields.push("category".to_string());
                }
                if pack.summary.to_ascii_lowercase().contains(needle) {
                    matched_fields.push("summary".to_string());
                }
                if pack
                    .exports
                    .commands
                    .iter()
                    .any(|value| value.to_ascii_lowercase().contains(needle))
                {
                    matched_fields.push("commands".to_string());
                }
                if pack
                    .exports
                    .themes
                    .iter()
                    .any(|value| value.to_ascii_lowercase().contains(needle))
                {
                    matched_fields.push("themes".to_string());
                }
                if pack
                    .exports
                    .providers
                    .iter()
                    .any(|value| value.to_ascii_lowercase().contains(needle))
                {
                    matched_fields.push("providers".to_string());
                }
                if matched_fields.is_empty() {
                    return None;
                }
            } else {
                matched_fields.push("all".to_string());
            }
            Some(PluginSearchResult {
                execution_model: plugin_execution_model(&pack).to_string(),
                externalization_class: plugin_externalization_class(&pack).to_string(),
                readiness: plugin_readiness_profile(&pack),
                pack,
                matched_fields,
                source: source.clone(),
                trust_source: trust_source.clone(),
            })
        })
        .collect()
}
pub fn plugin_bundle_status_json() -> anyhow::Result<String> {
    Ok(serde_json::to_string_pretty(&plugin_bundle_status())?)
}
pub fn plugin_bundle_status_text() -> String {
    let status = plugin_bundle_status();
    let mut out = String::new();
    if status.trust_source == "official_bundle" {
        out.push_str("Official Winuxsh plugin bundle\n");
    } else {
        out.push_str("Winuxsh plugin bundle status\n");
    }
    out.push_str(&format!("State: {}\n", status.state));
    out.push_str(&format!("Bundle: {}\n", status.bundle));
    out.push_str(&format!("Source: {}\n", status.source));
    out.push_str(&format!("Trust source: {}\n", status.trust_source));
    if let Some(version) = &status.active_version {
        out.push_str(&format!("Active version: {}\n", version));
    }
    if let Some(api) = &status.active_api {
        out.push_str(&format!("Active API: {}\n", api));
    }
    if let Some(path) = &status.active_path {
        out.push_str(&format!("Active path: {}\n", path.display()));
    }
    out.push_str(&format!("Bundle root: {}\n", status.bundle_root.display()));
    out.push_str(&format!("Lock file: {}\n", status.lock_path.display()));
    out.push_str(&format!("Message: {}\n", status.message));
    for err in &status.candidate_errors {
        out.push_str(&format!("Candidate error: {}\n", err));
    }
    out
}
fn plugin_bundle_status() -> PluginBundleStatus {
    let bundle_root = official_bundle_root();
    let lock_path = plugin_lock_path();
    let mut candidate_errors = Vec::new();
    match active_plugin_inventory_result() {
        Ok(inventory) => {
            let installed = inventory.path.is_some();
            let active_path = inventory.path.clone();
            let message = plugin_bundle_status_message(&inventory, installed).to_string();
            PluginBundleStatus {
                state: if installed {
                    "installed"
                } else {
                    "compiled_fallback"
                }
                .to_string(),
                bundle: inventory.bundle,
                source: inventory.source,
                trust_source: inventory.trust_source,
                active_version: Some(inventory.version),
                active_api: Some(inventory.api),
                min_winuxsh: Some(inventory.min_winuxsh),
                current_path: active_path.clone(),
                version_path: active_path.clone(),
                active_path,
                bundle_root,
                lock_path,
                message,
                candidate_errors,
            }
        }
        Err(err) => {
            candidate_errors.push(err.to_string());
            PluginBundleStatus {
                state: "compiled_fallback".to_string(),
                bundle: OFFICIAL_BUNDLE_NAME.to_string(),
                source: "compiled_fallback".to_string(),
                trust_source: "official_bundle".to_string(),
                active_version: Some(OFFICIAL_BUNDLE_VERSION.to_string()),
                active_api: Some(PLUGIN_BUNDLE_API_VERSION.to_string()),
                min_winuxsh: Some(env!("CARGO_PKG_VERSION").to_string()),
                active_path: None,
                current_path: None,
                version_path: None,
                bundle_root,
                lock_path,
                message: "using compiled fallback inventory".to_string(),
                candidate_errors,
            }
        }
    }
}
fn plugin_bundle_status_message(inventory: &PluginInventory, installed: bool) -> &'static str {
    if !installed {
        return "using compiled fallback inventory";
    }
    match inventory.trust_source.as_str() {
        "official_bundle" => "using installed official bundle",
        "local_override" => "using local override bundle",
        "external_bundle" => "using external bundle override (review-only)",
        _ => "using plugin bundle",
    }
}
pub fn plugin_doctor_report(config: &PluginConfig) -> PluginDoctorReport {
    let state = effective_plugin_state(config);
    let inventory = active_plugin_inventory();
    let source = inventory.source.clone();
    let trust_source = inventory.trust_source.clone();
    let mut packs = Vec::new();
    for pack in inventory.packs {
        if !state.is_enabled(&pack.name) {
            continue;
        }
        let missing = missing_required_binaries(&pack.required_binaries);
        let status = if missing.is_empty() { "ok" } else { "warning" }.to_string();
        packs.push(PluginDoctorPack {
            name: pack.name.clone(),
            kind: pack.kind,
            execution_model: plugin_execution_model(&pack).to_string(),
            externalization_class: plugin_externalization_class(&pack).to_string(),
            readiness: plugin_readiness_profile(&pack),
            status,
            enabled: true,
            missing_required_binaries: missing,
        });
    }
    let ok = packs.iter().all(|pack| pack.status == "ok");
    PluginDoctorReport {
        ok,
        status: if ok { "ok" } else { "warnings" }.to_string(),
        enabled: config.enabled,
        source,
        trust_source,
        packs,
    }
}
pub fn plugin_doctor_text(report: &PluginDoctorReport) -> String {
    let mut out = String::new();
    out.push_str("Winuxsh plugin doctor\n");
    out.push_str(&format!("Status: {}\n", report.status));
    out.push_str(&format!("Source: {}\n", report.source));
    out.push_str(&format!("Trust source: {}\n", report.trust_source));
    for pack in &report.packs {
        out.push_str(&format!(
            "- {} kind={} status={} execution={} externalization={} target={} shell_mutating={} fallback={}",
            pack.name,
            pack.kind.as_str(),
            pack.status,
            pack.execution_model,
            pack.externalization_class,
            pack.readiness.target_runtime,
            if pack.readiness.shell_mutating {
                "yes"
            } else {
                "no"
            },
            if pack.readiness.fallback_needed {
                "yes"
            } else {
                "no"
            }
        ));
        if !pack.missing_required_binaries.is_empty() {
            out.push_str(&format!(
                " missing required binaries: {}",
                pack.missing_required_binaries.join(",")
            ));
        }
        out.push('\n');
    }
    out
}
pub fn plugin_permission_review(
    name: &str,
    config: &PluginConfig,
) -> anyhow::Result<PluginPermissionReview> {
    let inventory = active_plugin_inventory();
    let trust_source = plugin_trust_source(&inventory).to_string();
    let pack = inventory
        .packs
        .into_iter()
        .find(|pack| pack.name.eq_ignore_ascii_case(name))
        .ok_or_else(|| anyhow!("unknown plugin '{}'", name))?;
    let state = effective_plugin_state(config);
    let mut notes = Vec::new();
    if pack.kind == PluginKind::Source {
        notes.push(
            "Source pack; startup code is sourced into the current interactive shell session."
                .to_string(),
        );
    } else if pack.kind == PluginKind::Bridge {
        notes.push(
            "Bridge pack; plugin-owned identity is wired to a host-owned API surface.".to_string(),
        );
    }
    match plugin_externalization_class(&pack) {
        "declarative_asset" => notes.push(
            "Declarative asset pack; no plugin runtime code executes for this pack.".to_string(),
        ),
        "mixed_declarative_native" => notes.push(
            "Mixed pack: static bundle assets are declarative, while dynamic behavior remains Winuxsh-owned.".to_string(),
        ),
        "pure_provider_candidate" => notes.push(
            "Provider candidate; process provider binding exists for command-not-found while a stable provider ABI is finalized.".to_string(),
        ),
        "shell_effect_candidate" => notes.push(
            "Shell-effect pack remains host-owned until env/cwd/history effects are explicit and permissioned.".to_string(),
        ),
        "shell_source" => notes.push(
            "Shell source pack; code runs in the current interactive session after plugin review and enablement.".to_string(),
        ),
        _ => {}
    }
    if trust_source == "external_bundle" {
        notes.push(
            "External bundle install is review-only until registry trust policy is implemented."
                .to_string(),
        );
    }
    let install_command = if trust_source == "external_bundle" {
        "unavailable: external bundle install requires registry trust policy".to_string()
    } else {
        format!("winuxsh plugin install {}", pack.name)
    };
    Ok(PluginPermissionReview {
        plugin: pack.name.clone(),
        kind: pack.kind,
        execution_model: plugin_execution_model(&pack).to_string(),
        externalization_class: plugin_externalization_class(&pack).to_string(),
        readiness: plugin_readiness_profile(&pack),
        trust_source,
        currently_enabled: state.is_enabled(&pack.name),
        permissions: pack
            .permissions
            .iter()
            .map(|token| permission_detail(token))
            .collect(),
        missing_required_binaries: missing_required_binaries(&pack.required_binaries),
        install_command,
        notes,
    })
}
pub fn plugin_permission_review_text(review: &PluginPermissionReview) -> String {
    let mut out = String::new();
    out.push_str(&format!("Plugin permission review: {}\n", review.plugin));
    out.push_str(&format!("Kind: {}\n", review.kind.as_str()));
    out.push_str(&format!("Execution model: {}\n", review.execution_model));
    out.push_str(&format!(
        "Externalization class: {}\n",
        review.externalization_class
    ));
    push_readiness_text(&mut out, &review.readiness);
    out.push_str(&format!("Trust source: {}\n", review.trust_source));
    out.push_str(&format!(
        "Currently enabled: {}\n",
        review.currently_enabled
    ));
    for item in &review.permissions {
        out.push_str(&format!(
            "{} risk={} scope={}\n",
            item.token, item.risk, item.scope
        ));
        out.push_str(&format!("  {}\n", item.description));
    }
    if !review.missing_required_binaries.is_empty() {
        out.push_str(&format!(
            "missing required binaries: {}\n",
            review.missing_required_binaries.join(",")
        ));
    }
    for note in &review.notes {
        out.push_str(note);
        out.push('\n');
    }
    out.push_str(&format!("Install command: {}\n", review.install_command));
    out
}
fn plugin_trust_source(inventory: &PluginInventory) -> &str {
    inventory.trust_source.as_str()
}
fn plugin_trust_source_for(bundle: &str, source: &str) -> &'static str {
    if bundle != OFFICIAL_BUNDLE_NAME {
        return "external_bundle";
    }
    match source {
        "compiled_fallback" | "app_bundle" | "user_bundle" => "official_bundle",
        _ => "local_override",
    }
}
fn permission_detail(token: &str) -> PermissionReviewItem {
    let (risk, scope, description) = if let Some(command) = token.strip_prefix("process:run:") {
        (
            "high",
            "process",
            format!("May execute the native command '{}'.", command),
        )
    } else if token == "shell:source" {
        (
            "high",
            "shell",
            "May source bundle-owned Winuxsh shell code into the current interactive session."
                .to_string(),
        )
    } else if token == "cwd:read" {
        (
            "low",
            "cwd",
            "May read the current working directory.".to_string(),
        )
    } else if token == "command:diagnose" {
        (
            "low",
            "command",
            "May inspect missing command context to generate suggestions.".to_string(),
        )
    } else if let Some(name) = plugin_env_read_permission_name(token) {
        (
            "low",
            "environment",
            format!("May read the '{}' environment variable.", name),
        )
    } else if token.starts_with("env:") {
        (
            "medium",
            "environment",
            "May modify process environment for this shell session.".to_string(),
        )
    } else {
        (
            "medium",
            "shell",
            format!("Requests permission token '{}'.", token),
        )
    };
    PermissionReviewItem {
        token: token.to_string(),
        risk: risk.to_string(),
        scope: scope.to_string(),
        description,
    }
}
fn plugin_env_read_permission_name(permission: &str) -> Option<&str> {
    let name = permission.strip_prefix("env:read:")?;
    if name.is_empty()
        || name.len() > 256
        || !name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
    {
        return None;
    }
    Some(name)
}
pub fn apply_plugin_bundle_update_from_path(
    bundle_name: &str,
    source_path: &Path,
    expected_checksum: Option<&str>,
) -> anyhow::Result<PluginBundleUpdateSummary> {
    if bundle_name != OFFICIAL_BUNDLE_NAME {
        anyhow::bail!(
            "unsupported bundle '{}'; only {} is supported",
            bundle_name,
            OFFICIAL_BUNDLE_NAME
        );
    }
    if !source_path.exists() {
        anyhow::bail!("bundle source {} does not exist", source_path.display());
    }
    let checksum_sha256 = if source_path.is_file() {
        let actual = file_sha256(source_path)?;
        if let Some(expected) = expected_checksum {
            if !actual.eq_ignore_ascii_case(expected.trim()) {
                anyhow::bail!(
                    "checksum mismatch for {}: expected {}, got {}",
                    source_path.display(),
                    expected,
                    actual
                );
            }
        }
        Some(actual)
    } else {
        if expected_checksum.is_some() {
            anyhow::bail!("--checksum is only supported for archive bundle sources");
        }
        None
    };
    let root = official_bundle_root().join(bundle_name);
    fs::create_dir_all(&root)?;
    let staging = unique_bundle_staging_path(&root);
    if staging.exists() {
        fs::remove_dir_all(&staging)?;
    }
    fs::create_dir_all(&staging)?;
    let update_result = (|| -> anyhow::Result<PluginBundleUpdateSummary> {
        if source_path.is_dir() {
            copy_dir_recursive(source_path, &staging)?;
        } else if is_zip_path(source_path) {
            extract_zip_archive(source_path, &staging)?;
        } else {
            anyhow::bail!(
                "bundle source {} is not a directory or .zip archive",
                source_path.display()
            );
        }
        let inventory = load_official_bundle_inventory_from_path(&staging, "staging")?;
        validate_bundle_inventory_for_update(
            &inventory,
            &staging,
            source_path,
            expected_checksum,
            checksum_sha256.as_deref(),
        )?;
        let installed_path = root.join(safe_path_component(&inventory.version));
        if installed_path.exists() {
            fs::remove_dir_all(&installed_path)?;
        }
        if fs::rename(&staging, &installed_path).is_err() {
            copy_dir_recursive(&staging, &installed_path)?;
            fs::remove_dir_all(&staging)?;
        }
        let lock_path = plugin_lock_path();
        if let Some(parent) = lock_path.parent() {
            fs::create_dir_all(parent)?;
        }
        let previous_path = read_plugin_lock(&lock_path)
            .ok()
            .map(|lock| lock.active_path);
        write_plugin_lock(
            &lock_path,
            &PluginLockToml {
                bundle: bundle_name.to_string(),
                version: inventory.version.clone(),
                active_path: installed_path.clone(),
                previous_path: previous_path.clone(),
                checksum_sha256: checksum_sha256.clone(),
            },
        )?;
        Ok(PluginBundleUpdateSummary {
            bundle: bundle_name.to_string(),
            version: inventory.version,
            installed_path,
            previous_path,
            lock_path,
            checksum_sha256,
        })
    })();
    if update_result.is_err() && staging.exists() {
        let _ = fs::remove_dir_all(&staging);
    }
    update_result
}
pub fn apply_plugin_bundle_rollback(
    bundle_name: &str,
) -> anyhow::Result<PluginBundleRollbackSummary> {
    if bundle_name != OFFICIAL_BUNDLE_NAME {
        anyhow::bail!(
            "unsupported bundle '{}'; only {} is supported",
            bundle_name,
            OFFICIAL_BUNDLE_NAME
        );
    }
    let lock_path = plugin_lock_path();
    let lock = read_plugin_lock(&lock_path)?;
    let previous = lock
        .previous_path
        .ok_or_else(|| anyhow!("no previous bundle path recorded for '{}'", bundle_name))?;
    if !previous.exists() {
        anyhow::bail!("previous bundle path {} does not exist", previous.display());
    }
    let inventory = load_official_bundle_inventory_from_path(&previous, "user_bundle")?;
    write_plugin_lock(
        &lock_path,
        &PluginLockToml {
            bundle: bundle_name.to_string(),
            version: inventory.version.clone(),
            active_path: previous.clone(),
            previous_path: Some(lock.active_path.clone()),
            checksum_sha256: None,
        },
    )?;
    Ok(PluginBundleRollbackSummary {
        bundle: bundle_name.to_string(),
        version: inventory.version,
        active_path: previous,
        previous_path: Some(lock.active_path),
        lock_path,
    })
}
fn validate_bundle_inventory_for_update(
    inventory: &PluginInventory,
    root: &Path,
    source_path: &Path,
    expected_checksum: Option<&str>,
    actual_checksum: Option<&str>,
) -> anyhow::Result<()> {
    if inventory.bundle != OFFICIAL_BUNDLE_NAME {
        anyhow::bail!(
            "bundle name '{}' does not match {}",
            inventory.bundle,
            OFFICIAL_BUNDLE_NAME
        );
    }
    if inventory.api != PLUGIN_BUNDLE_API_VERSION {
        anyhow::bail!(
            "bundle api '{}' does not match {}",
            inventory.api,
            PLUGIN_BUNDLE_API_VERSION
        );
    }
    if semver_gt(&inventory.min_winuxsh, env!("CARGO_PKG_VERSION")) {
        anyhow::bail!(
            "bundle '{}' requires Winuxsh >= {}",
            inventory.bundle,
            inventory.min_winuxsh
        );
    }
    for pack in &inventory.packs {
        if pack.bundle != inventory.bundle {
            anyhow::bail!("pack '{}' belongs to bundle '{}'", pack.name, pack.bundle);
        }
        if pack.api != PLUGIN_API_VERSION {
            anyhow::bail!(
                "pack '{}' api '{}' does not match {}",
                pack.name,
                pack.api,
                PLUGIN_API_VERSION
            );
        }
        validate_provider_exports_contract(pack)?;
        match pack.kind {
            PluginKind::Builtin => {}
            PluginKind::Source => validate_source_pack_contract(pack, root)?,
            PluginKind::Bridge => validate_bridge_pack_contract(pack)?,
            PluginKind::Process => validate_process_pack_contract(pack)?,
        }
    }
    validate_bundle_index_for_update(
        inventory,
        root,
        source_path,
        expected_checksum,
        actual_checksum,
    )?;
    Ok(())
}
fn validate_bundle_index_for_update(
    inventory: &PluginInventory,
    root: &Path,
    source_path: &Path,
    expected_checksum: Option<&str>,
    actual_checksum: Option<&str>,
) -> anyhow::Result<()> {
    let index_path = root.join("index.toml");
    let index_text = fs::read_to_string(&index_path)
        .with_context(|| format!("failed to read {}", index_path.display()))?;
    let index: BundleIndexToml = toml::from_str(&index_text)
        .with_context(|| format!("failed to parse {}", index_path.display()))?;
    if index.schema != PLUGIN_INDEX_SCHEMA {
        anyhow::bail!(
            "index.toml schema '{}' does not match {}",
            index.schema,
            PLUGIN_INDEX_SCHEMA
        );
    }
    if index.bundle != inventory.bundle {
        anyhow::bail!(
            "index.toml bundle '{}' does not match {}",
            index.bundle,
            inventory.bundle
        );
    }
    if index.version != inventory.version {
        anyhow::bail!(
            "index.toml version '{}' does not match {}",
            index.version,
            inventory.version
        );
    }
    if index.bundle_api != inventory.api {
        anyhow::bail!(
            "index.toml bundle_api '{}' does not match {}",
            index.bundle_api,
            inventory.api
        );
    }
    if index.min_winuxsh != inventory.min_winuxsh {
        anyhow::bail!(
            "index.toml min_winuxsh '{}' does not match {}",
            index.min_winuxsh,
            inventory.min_winuxsh
        );
    }
    validate_bundle_index_release(
        &index.release,
        &inventory.version,
        source_path,
        expected_checksum,
        actual_checksum,
    )?;
    let bundle_path = root.join("bundle.toml");
    let bundle_text = fs::read_to_string(&bundle_path)
        .with_context(|| format!("failed to read {}", bundle_path.display()))?;
    let bundle: BundleToml = toml::from_str(&bundle_text)
        .with_context(|| format!("failed to parse {}", bundle_path.display()))?;
    let legacy_packs = load_legacy_bundle_pack_records(root, &bundle)?;
    validate_bundle_index_packs(&index.packs, &legacy_packs)?;
    Ok(())
}
fn validate_bundle_index_release(
    release: &BundleIndexReleaseToml,
    version: &str,
    source_path: &Path,
    expected_checksum: Option<&str>,
    actual_checksum: Option<&str>,
) -> anyhow::Result<()> {
    let expected_artifact = format!("{OFFICIAL_BUNDLE_NAME}-{version}.zip");
    if release.artifact != expected_artifact {
        anyhow::bail!(
            "index.toml release.artifact '{}' does not match {}",
            release.artifact,
            expected_artifact
        );
    }
    let expected_checksum_file = format!("{expected_artifact}.sha256");
    if release.checksum != expected_checksum_file {
        anyhow::bail!(
            "index.toml release.checksum '{}' does not match {}",
            release.checksum,
            expected_checksum_file
        );
    }
    if release.checksum_algorithm != "sha256" {
        anyhow::bail!(
            "index.toml release.checksum_algorithm '{}' is not supported",
            release.checksum_algorithm
        );
    }
    if !release.checksum_required {
        anyhow::bail!("index.toml release.checksum_required must be true");
    }
    if release.signature != PLUGIN_INDEX_SIGNATURE_POLICY {
        anyhow::bail!(
            "index.toml release.signature '{}' does not match {}",
            release.signature,
            PLUGIN_INDEX_SIGNATURE_POLICY
        );
    }
    if source_path.is_file() && expected_checksum.is_none() {
        anyhow::bail!(
            "index.toml requires checksum verification for {}; pass --checksum or --checksum-file",
            release.artifact
        );
    }
    if source_path.is_file() && actual_checksum.is_none() {
        anyhow::bail!("archive checksum was not computed for {}", release.artifact);
    }
    Ok(())
}
fn validate_bundle_index_packs(
    index_packs: &[BundleIndexPackToml],
    inventory_packs: &[PluginPackRecord],
) -> anyhow::Result<()> {
    if index_packs.len() != inventory_packs.len() {
        anyhow::bail!(
            "index.toml pack count {} does not match bundle manifest count {}",
            index_packs.len(),
            inventory_packs.len()
        );
    }
    for (index_pack, pack) in index_packs.iter().zip(inventory_packs) {
        if index_pack.name != pack.name {
            anyhow::bail!(
                "index.toml pack order drift: '{}' does not match '{}'",
                index_pack.name,
                pack.name
            );
        }
        if index_pack.version != pack.version {
            anyhow::bail!("index.toml {}.version must match manifest", pack.name);
        }
        if index_pack.api != pack.api {
            anyhow::bail!("index.toml {}.api must match manifest", pack.name);
        }
        if index_pack.kind != pack.kind {
            anyhow::bail!("index.toml {}.kind must match manifest", pack.name);
        }
        if index_pack.category != pack.category {
            anyhow::bail!("index.toml {}.category must match manifest", pack.name);
        }
        if index_pack.summary != pack.summary {
            anyhow::bail!("index.toml {}.summary must match manifest", pack.name);
        }
        if index_pack.default != pack.default {
            anyhow::bail!("index.toml {}.default must match manifest", pack.name);
        }
        if index_pack.permissions != pack.permissions {
            anyhow::bail!("index.toml {}.permissions must match manifest", pack.name);
        }
        if index_pack.required_binaries != pack.required_binaries {
            anyhow::bail!(
                "index.toml {}.required_binaries must match manifest",
                pack.name
            );
        }
    }
    Ok(())
}
fn validate_provider_exports_contract(pack: &PluginPackRecord) -> anyhow::Result<()> {
    if pack.exports.providers.is_empty() {
        return Ok(());
    }
    let mut seen = BTreeSet::new();
    for provider in &pack.exports.providers {
        if !seen.insert(provider.as_str()) {
            anyhow::bail!(
                "pack '{}' exports provider '{}' more than once",
                pack.name,
                provider
            );
        }
        if provider != COMMAND_NOT_FOUND_PROVIDER {
            anyhow::bail!(
                "pack '{}' exports unknown provider '{}'",
                pack.name,
                provider
            );
        }
    }
    match pack.kind {
        PluginKind::Builtin => {
            if pack.name != COMMAND_NOT_FOUND_PROVIDER {
                anyhow::bail!(
                    "pack '{}' must not export provider '{}'; that provider is reserved for '{}'",
                    pack.name,
                    COMMAND_NOT_FOUND_PROVIDER,
                    COMMAND_NOT_FOUND_PROVIDER
                );
            }
        }
        PluginKind::Process => {}
        PluginKind::Bridge => {
            if pack.name != COMMAND_NOT_FOUND_PROVIDER {
                anyhow::bail!(
                    "bridge pack '{}' must not export provider '{}'; that provider is reserved for '{}'",
                    pack.name,
                    COMMAND_NOT_FOUND_PROVIDER,
                    COMMAND_NOT_FOUND_PROVIDER
                );
            }
        }
        PluginKind::Source => {
            anyhow::bail!(
                "source pack '{}' must not export providers until sourced provider hooks are implemented",
                pack.name
            );
        }
    }
    if !pack
        .permissions
        .iter()
        .any(|permission| permission == "command:diagnose")
    {
        anyhow::bail!(
            "pack '{}' provider '{}' requires permission 'command:diagnose'",
            pack.name,
            COMMAND_NOT_FOUND_PROVIDER
        );
    }
    Ok(())
}
fn validate_source_pack_contract(pack: &PluginPackRecord, root: &Path) -> anyhow::Result<()> {
    let source = pack
        .source
        .as_ref()
        .ok_or_else(|| anyhow!("source pack '{}' is missing [source]", pack.name))?;
    if !pack
        .permissions
        .iter()
        .any(|permission| permission == "shell:source")
    {
        anyhow::bail!(
            "source pack '{}' must declare permission 'shell:source'",
            pack.name
        );
    }
    let path = safe_bundle_relative_path(root, &source.entry).ok_or_else(|| {
        anyhow!(
            "source pack '{}' entry '{}' must be a bundle-local relative path",
            pack.name,
            source.entry
        )
    })?;
    if path.extension().and_then(|value| value.to_str()) != Some("winux") {
        anyhow::bail!(
            "source pack '{}' entry '{}' must end in .winux",
            pack.name,
            source.entry
        );
    }
    if !path.is_file() {
        anyhow::bail!(
            "source pack '{}' entry '{}' does not exist",
            pack.name,
            source.entry
        );
    }
    for hook in &pack.exports.hooks {
        if !SOURCE_PLUGIN_HOOKS.contains(&hook.as_str()) {
            anyhow::bail!(
                "source pack '{}' exports unsupported hook '{}'",
                pack.name,
                hook
            );
        }
    }
    Ok(())
}
fn validate_bridge_pack_contract(pack: &PluginPackRecord) -> anyhow::Result<()> {
    if pack.source.is_some() || pack.process.is_some() {
        anyhow::bail!(
            "bridge pack '{}' must not declare source or process runtime blocks",
            pack.name
        );
    }
    if !pack.exports.hooks.is_empty() || !pack.exports.commands.is_empty() {
        anyhow::bail!(
            "bridge pack '{}' must not export shell hooks or commands",
            pack.name
        );
    }
    Ok(())
}
fn validate_process_pack_contract(pack: &PluginPackRecord) -> anyhow::Result<()> {
    let process = pack
        .process
        .as_ref()
        .ok_or_else(|| anyhow!("process pack '{}' is missing [process]", pack.name))?;
    if pack.default {
        anyhow::bail!("process pack '{}' must be explicit opt-in", pack.name);
    }
    if process.protocol != PROCESS_PLUGIN_PROTOCOL {
        anyhow::bail!(
            "process pack '{}' protocol '{}' does not match {}",
            pack.name,
            process.protocol,
            PROCESS_PLUGIN_PROTOCOL
        );
    }
    let permission = format!("process:run:{}", process.command);
    if !pack
        .permissions
        .iter()
        .any(|candidate| candidate == &permission)
    {
        anyhow::bail!(
            "process pack '{}' must declare permission '{}'",
            pack.name,
            permission
        );
    }
    if process.timeout_millis == 0 {
        anyhow::bail!("process pack '{}' timeout_millis must be > 0", pack.name);
    }
    Ok(())
}
#[derive(Debug, Clone)]
pub struct SourcePluginScript {
    pub pack: String,
    pub path: PathBuf,
    pub bundle_root: PathBuf,
}
pub fn source_plugin_scripts(state: &PluginRuntimeState) -> Vec<SourcePluginScript> {
    source_plugin_scripts_for_hook(state, "startup")
}
pub fn source_plugin_scripts_for_hook(
    state: &PluginRuntimeState,
    hook_name: &str,
) -> Vec<SourcePluginScript> {
    let inventory = active_plugin_inventory();
    let Some(root) = inventory.path.as_ref() else {
        return Vec::new();
    };
    inventory
        .packs
        .into_iter()
        .filter_map(|pack| {
            if pack.kind != PluginKind::Source || !state.is_enabled(&pack.name) {
                return None;
            }
            if !source_plugin_exports_hook(&pack.exports.hooks, hook_name) {
                return None;
            }
            let source = pack.source?;
            let path = safe_bundle_relative_path(root, &source.entry)?;
            path.is_file().then_some(SourcePluginScript {
                pack: pack.name,
                path,
                bundle_root: root.clone(),
            })
        })
        .collect()
}
fn source_plugin_exports_hook(hooks: &[String], hook_name: &str) -> bool {
    if hook_name == "startup" && hooks.is_empty() {
        return true;
    }
    hooks.iter().any(|hook| hook == hook_name)
}
pub fn plugin_aliases(pack_name: &str) -> Option<Vec<(String, String)>> {
    let inventory = active_plugin_inventory();
    if let Some(root) = &inventory.path {
        let pack = inventory
            .packs
            .iter()
            .find(|pack| pack.name == pack_name && pack.exports.aliases)?;
        if inventory.bundle == OFFICIAL_BUNDLE_NAME {
            if let Some(aliases) = load_bundle_aliases_from_path(root, pack) {
                return Some(aliases);
            }
            return compiled_plugin_aliases(pack_name);
        }
        return load_bundle_aliases_from_path(root, pack);
    }
    compiled_plugin_aliases(pack_name)
}
fn compiled_plugin_aliases(pack_name: &str) -> Option<Vec<(String, String)>> {
    let aliases: &[(&str, &str)] = match pack_name {
        "git" => &[
            ("g", "git"),
            ("gst", "git status"),
            ("gco", "git checkout"),
            ("gcm", "git commit"),
            ("gp", "git push"),
        ],
        "docker" => &[
            ("d", "docker"),
            ("dps", "docker ps"),
            ("dc", "docker compose"),
        ],
        "kubectl" => &[
            ("k", "kubectl"),
            ("kgp", "kubectl get pods"),
            ("kgs", "kubectl get services"),
        ],
        "npm" => &[("nr", "npm run"), ("ni", "npm install"), ("nt", "npm test")],
        _ => return None,
    };
    Some(
        aliases
            .iter()
            .map(|(name, value)| ((*name).to_string(), (*value).to_string()))
            .collect(),
    )
}
fn load_bundle_aliases_from_path(
    root: &Path,
    pack: &PluginPackRecord,
) -> Option<Vec<(String, String)>> {
    let path = bundle_asset_path(root, "aliases", &pack.name, "toml")?;
    let text = fs::read_to_string(path).ok()?;
    let parsed: BundleAliasesToml = toml::from_str(&text).ok()?;
    Some(parsed.aliases.into_iter().collect())
}
pub fn plugin_completion_defs(state: &PluginRuntimeState) -> Vec<CommandDef> {
    let inventory = active_plugin_inventory();
    let mut defs = Vec::new();
    let mut requested = BTreeSet::new();
    let mut loaded = BTreeSet::new();
    if let Some(root) = &inventory.path {
        for pack in inventory
            .packs
            .iter()
            .filter(|pack| state.is_enabled(&pack.name))
        {
            for name in &pack.exports.completions {
                requested.insert(name.clone());
                if let Some(def) = load_bundle_completion_def_from_path(root, name) {
                    defs.push(def);
                    loaded.insert(name.clone());
                }
            }
        }
    } else {
        for pack in inventory
            .packs
            .iter()
            .filter(|pack| state.is_enabled(&pack.name))
        {
            requested.extend(pack.exports.completions.iter().cloned());
        }
    }
    if inventory.bundle == OFFICIAL_BUNDLE_NAME {
        for name in requested {
            if loaded.contains(&name) {
                continue;
            }
            if let Some(def) = compiled_completion_def(&name) {
                defs.push(def);
            }
        }
    }
    defs
}
fn load_bundle_completion_def_from_path(root: &Path, name: &str) -> Option<CommandDef> {
    let path = bundle_asset_path(root, "completions", name, "toml")?;
    let text = fs::read_to_string(path).ok()?;
    toml::from_str(&text).ok()
}
fn compiled_completion_def(name: &str) -> Option<CommandDef> {
    match name {
        "git" => Some(git_completion_def()),
        "docker" => Some(simple_command_def("docker", "Docker command")),
        "kubectl" => Some(simple_command_def("kubectl", "Kubernetes CLI")),
        "npm" => Some(simple_command_def("npm", "Node package manager")),
        _ => None,
    }
}

fn git_completion_def() -> CommandDef {
    CommandDef {
        command: "git".to_string(),
        description: Some("Git command".to_string()),
        flags: Vec::<FlagDef>::new(),
        subcommands: vec![
            simple_subcommand("add", "Add file contents to the index"),
            simple_subcommand("branch", "List, create, or delete branches"),
            simple_subcommand("checkout", "Switch branches or restore working tree files"),
            simple_subcommand("clone", "Clone a repository"),
            subcommand(
                "commit",
                "Record changes to the repository",
                vec![
                    long_flag(
                        "--message",
                        "Use the given message as the commit message",
                        true,
                    ),
                    long_flag("--amend", "Amend the tip of the current branch", false),
                    long_flag(
                        "--no-verify",
                        "Bypass pre-commit and commit-msg hooks",
                        false,
                    ),
                ],
            ),
            simple_subcommand("diff", "Show changes between commits or the working tree"),
            simple_subcommand("fetch", "Download objects and refs from another repository"),
            simple_subcommand("log", "Show commit logs"),
            simple_subcommand("pull", "Fetch from and integrate with another repository"),
            subcommand(
                "push",
                "Update remote refs along with associated objects",
                vec![
                    long_flag("--force", "Force updates", false),
                    long_flag(
                        "--force-with-lease",
                        "Force updates if the remote ref is unchanged",
                        false,
                    ),
                ],
            ),
            simple_subcommand("rebase", "Reapply commits on top of another base tip"),
            simple_subcommand("status", "Show the working tree status"),
        ],
    }
}

fn simple_subcommand(name: &str, description: &str) -> SubcommandDef {
    subcommand(name, description, Vec::new())
}

fn subcommand(name: &str, description: &str, flags: Vec<FlagDef>) -> SubcommandDef {
    SubcommandDef {
        name: name.to_string(),
        description: Some(description.to_string()),
        flags,
    }
}

fn long_flag(long: &str, description: &str, takes_value: bool) -> FlagDef {
    FlagDef {
        short: None,
        long: Some(long.to_string()),
        description: Some(description.to_string()),
        takes_value,
        values_source: None,
    }
}

fn simple_command_def(command: &str, description: &str) -> CommandDef {
    CommandDef {
        command: command.to_string(),
        description: Some(description.to_string()),
        flags: Vec::<FlagDef>::new(),
        subcommands: Vec::<SubcommandDef>::new(),
    }
}
pub fn plugin_prompt_preset(name: &str) -> Option<PromptPresetAsset> {
    let inventory = active_plugin_inventory();
    let root = inventory.path.as_ref()?;
    let parsed = load_bundle_prompt_segments_from_path(root)?;
    parsed
        .presets
        .into_iter()
        .find(|(candidate, _)| candidate.eq_ignore_ascii_case(name))
        .map(|(_, preset)| resolve_prompt_segment_refs(preset, &parsed.segments))
}
fn load_bundle_prompt_segments_from_path(root: &Path) -> Option<BundlePromptSegmentsToml> {
    let path = bundle_asset_path(root, "prompts", "segments", "toml")?;
    let text = fs::read_to_string(path).ok()?;
    toml::from_str(&text).ok()
}
fn resolve_prompt_segment_refs(
    preset: BundlePromptPresetToml,
    segments: &BTreeMap<String, BundlePromptSegmentToml>,
) -> PromptPresetAsset {
    let resolve = |refs: Vec<String>| -> Vec<String> {
        refs.into_iter()
            .map(|name| {
                segments
                    .get(&name)
                    .map(|segment| segment.id.clone())
                    .unwrap_or(name)
            })
            .collect()
    };
    PromptPresetAsset {
        left_elements: resolve(preset.left),
        right_elements: resolve(preset.right),
        separator: preset.separator,
        git_prompt_format: preset.git_prompt_format,
    }
}
pub fn plugin_theme_catalog_json() -> anyhow::Result<String> {
    Ok(serde_json::to_string_pretty(&plugin_theme_catalog())?)
}
pub fn plugin_theme_catalog_text() -> String {
    let mut out = String::new();
    out.push_str("Winuxsh themes\n");
    out.push_str("Resolution order: user > active bundle\n");
    for entry in plugin_theme_catalog() {
        out.push_str(&format!(
            "- {} source={} owner={}",
            entry.name, entry.source, entry.owner
        ));
        if let Some(bundle) = entry.bundle {
            out.push_str(&format!(" bundle={bundle}"));
        }
        if let Some(pack) = entry.pack {
            out.push_str(&format!(" pack={pack}"));
        }
        if let Some(path) = entry.path {
            out.push_str(&format!(" path={}", path.display()));
        }
        out.push_str(&format!(" trust_source={}", entry.trust_source));
        out.push('\n');
    }
    out
}
pub fn plugin_theme_catalog() -> Vec<PluginThemeCatalogEntry> {
    let mut entries = Vec::new();
    let mut seen = BTreeSet::new();

    for entry in crate::theme::user_theme_entries() {
        seen.insert(entry.name.to_ascii_lowercase());
        entries.push(PluginThemeCatalogEntry {
            name: entry.name,
            source: "user".to_string(),
            trust_source: "user_theme".to_string(),
            owner: "~/.winuxsh/themes".to_string(),
            bundle: None,
            pack: None,
            path: Some(entry.path),
        });
    }

    let inventory = active_plugin_inventory();
    if let Some(root) = &inventory.path {
        for pack in &inventory.packs {
            for theme_name in &pack.exports.themes {
                let Some(path) = bundle_asset_path(root, "themes", theme_name, "toml") else {
                    continue;
                };
                if crate::theme::load_theme_from_file(theme_name, &path).is_none() {
                    continue;
                }
                seen.insert(theme_name.to_ascii_lowercase());
                entries.push(PluginThemeCatalogEntry {
                    name: theme_name.clone(),
                    source: "bundle".to_string(),
                    trust_source: inventory.trust_source.clone(),
                    owner: format!("{}@{}", inventory.bundle, inventory.version),
                    bundle: Some(inventory.bundle.clone()),
                    pack: Some(pack.name.clone()),
                    path: Some(path),
                });
            }
        }
    }
    entries
}
pub fn plugin_theme(name: &str) -> Option<Theme> {
    let inventory = active_plugin_inventory();
    let root = inventory.path.as_ref()?;
    if !inventory.packs.iter().any(|pack| {
        pack.exports
            .themes
            .iter()
            .any(|theme| theme.eq_ignore_ascii_case(name))
    }) {
        return None;
    }
    load_bundle_theme_from_path(root, name)
}
pub fn plugin_theme_names() -> Vec<String> {
    active_plugin_inventory()
        .packs
        .into_iter()
        .flat_map(|pack| pack.exports.themes.into_iter())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}
fn load_bundle_theme_from_path(root: &Path, name: &str) -> Option<Theme> {
    let path = bundle_asset_path(root, "themes", name, "toml")?;
    crate::theme::load_theme_from_file(name, &path)
}
fn plugin_keybinding_metadata_lines(
    inventory: &PluginInventory,
    pack: &PluginPackRecord,
) -> Vec<String> {
    let Some(root) = &inventory.path else {
        return Vec::new();
    };
    pack.exports
        .keybindings
        .iter()
        .filter_map(|name| load_bundle_keybindings_from_path(root, name))
        .map(|metadata| {
            format!(
                "{} keymap={} bindings={} summary={}",
                metadata.name,
                metadata.keymap,
                metadata.bindings.len(),
                metadata.summary
            )
        })
        .collect()
}
fn load_bundle_keybindings_from_path(root: &Path, name: &str) -> Option<BundleKeybindingsToml> {
    let path = bundle_asset_path(root, "keybindings", name, "toml")?;
    let text = fs::read_to_string(path).ok()?;
    toml::from_str(&text).ok()
}
fn bundle_asset_path(
    root: &Path,
    logical_dir: &str,
    name: &str,
    extension: &str,
) -> Option<PathBuf> {
    if !safe_asset_name(name) {
        return None;
    }
    let layout = bundle_layout(root).ok()?;
    let dir = match logical_dir {
        "aliases" => layout.aliases_dir,
        "completions" => layout.completions_dir,
        "prompts" => layout.prompts_dir,
        "keybindings" => layout.keybindings_dir,
        "themes" => layout.themes_dir,
        _ => return None,
    };
    Some(root.join(dir).join(format!("{name}.{extension}")))
}
fn safe_bundle_relative_path(root: &Path, value: &str) -> Option<PathBuf> {
    let path = Path::new(value);
    if value.is_empty()
        || path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return None;
    }
    Some(root.join(path))
}
fn bundle_layout(root: &Path) -> anyhow::Result<BundleLayoutToml> {
    let text = fs::read_to_string(root.join("bundle.toml"))?;
    let bundle: BundleToml = toml::from_str(&text)?;
    Ok(bundle.layout)
}
fn safe_asset_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
}
fn missing_required_binaries(required: &[String]) -> Vec<String> {
    required
        .iter()
        .filter(|binary| resolve_binary(binary).is_none())
        .cloned()
        .collect()
}
fn resolve_binary(binary: &str) -> Option<PathBuf> {
    let path = Path::new(binary);
    if path.components().count() > 1 && path.is_file() {
        return Some(path.to_path_buf());
    }
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        let direct = dir.join(binary);
        if direct.is_file() {
            return Some(direct);
        }
        if cfg!(windows) && Path::new(binary).extension().is_none() {
            for ext in [".exe", ".cmd", ".bat", ".com"] {
                let candidate = dir.join(format!("{}{}", binary, ext));
                if candidate.is_file() {
                    return Some(candidate);
                }
            }
        }
    }
    None
}
fn env_path(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .map(|value| PathBuf::from(shell_path_to_host_path(value.to_string_lossy().as_ref())))
        .filter(|path| !path.as_os_str().is_empty())
}
fn official_bundle_root() -> PathBuf {
    env_path("WINUXSH_PLUGIN_BUNDLE_ROOT")
        .or_else(|| shell_home_dir().map(|home| home.join(".winuxsh").join("bundles")))
        .unwrap_or_else(|| PathBuf::from(".winuxsh-bundles"))
}
fn plugin_lock_path() -> PathBuf {
    env_path("WINUXSH_PLUGIN_LOCK")
        .or_else(|| shell_home_dir().map(|home| home.join(".winuxsh").join("plugin-lock.toml")))
        .unwrap_or_else(|| PathBuf::from("plugin-lock.toml"))
}
fn app_bundled_bundle_candidates() -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(path) = env_path("WINUXSH_APP_BUNDLE_PATH") {
        candidates.push(path);
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            candidates.push(parent.join("bundles").join(OFFICIAL_BUNDLE_NAME));
        }
    }
    candidates
}
fn read_plugin_lock(path: &Path) -> anyhow::Result<PluginLockToml> {
    let text =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    toml::from_str(&text).with_context(|| format!("failed to parse {}", path.display()))
}
fn write_plugin_lock(path: &Path, lock: &PluginLockToml) -> anyhow::Result<()> {
    let text = toml::to_string_pretty(lock)?;
    fs::write(path, text)?;
    Ok(())
}
fn file_sha256(path: &Path) -> anyhow::Result<String> {
    let mut file =
        fs::File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 8192];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}
fn is_zip_path(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.eq_ignore_ascii_case("zip"))
        .unwrap_or(false)
}
fn extract_zip_archive(archive_path: &Path, dest: &Path) -> anyhow::Result<()> {
    let file = fs::File::open(archive_path)?;
    let mut archive = zip::ZipArchive::new(file)
        .with_context(|| format!("failed to read zip archive {}", archive_path.display()))?;
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index)?;
        let enclosed = entry
            .enclosed_name()
            .ok_or_else(|| anyhow!("zip archive contains unsafe path '{}'", entry.name()))?
            .to_path_buf();
        let out_path = dest.join(enclosed);
        if entry.name().ends_with('/') {
            fs::create_dir_all(&out_path)?;
        } else {
            if let Some(parent) = out_path.parent() {
                fs::create_dir_all(parent)?;
            }
            let mut out_file = fs::File::create(&out_path)?;
            std::io::copy(&mut entry, &mut out_file)?;
        }
    }
    Ok(())
}
fn copy_dir_recursive(src: &Path, dest: &Path) -> anyhow::Result<()> {
    fs::create_dir_all(dest)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let path = entry.path();
        let target = dest.join(entry.file_name());
        if path.is_dir() {
            copy_dir_recursive(&path, &target)?;
        } else if path.is_file() {
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::copy(&path, &target)?;
        }
    }
    Ok(())
}
fn unique_bundle_staging_path(root: &Path) -> PathBuf {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    root.join(format!(".staging-{}-{stamp}", std::process::id()))
}
fn safe_path_component(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '.' || ch == '-' || ch == '_' {
                ch
            } else {
                '_'
            }
        })
        .collect()
}
fn semver_gt(left: &str, right: &str) -> bool {
    let left = parse_semver_core(left);
    let right = parse_semver_core(right);
    left > right
}
fn parse_semver_core(value: &str) -> (u64, u64, u64) {
    let mut parts = value.split('.');
    (
        parse_semver_part(parts.next()),
        parse_semver_part(parts.next()),
        parse_semver_part(parts.next()),
    )
}
fn parse_semver_part(value: Option<&str>) -> u64 {
    value
        .unwrap_or("0")
        .chars()
        .take_while(|ch| ch.is_ascii_digit())
        .collect::<String>()
        .parse()
        .unwrap_or(0)
}
