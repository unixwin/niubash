//! `niu plugin sync` — reconcile the declarative spec against the machine
//! (design §14.6.3; lazy.nvim `:Lazy sync` semantics).
//!
//! The spec (`plugins::spec`, `~/.niubash/plugins.toml`) declares *what
//! should exist*; the registry (`plugins::sources`) locks *what exists*
//! (commit + tree checksum). Sync is the only thing that moves the two
//! towards each other:
//!
//! 1. **declared, not installed** → fetch through the add pipeline
//!    (fetch gate only; the trust gate is never automatic — the new source
//!    lands untrusted and sync prints the exact `niu plugin trust <id>`);
//!    an install identity that is already registered is adopted, not
//!    refused;
//! 2. **declared, installed, trusted** → materialize the managed rc block
//!    / enabled tree *from the spec*, idempotently (`plugins::assets::
//!    materialize_spec_selection`, hand-added entries preserved);
//!    immediately before this pass, theme claims are reconciled to a
//!    single owner toward the rc's live state (niubash#168): a theme pick
//!    written to the rc + spec is the user's latest expressed choice, so a
//!    stale same-name claim is cleared — never re-routed over a working
//!    theme block — and sync prints what it reconciled;
//! 3. **installed, not declared** → suggest cleanup, never auto-delete
//!    (`--prune` removes them explicitly);
//! 4. **spec absent** → legacy imperative mode: nothing to reconcile; sync
//!    reports and suggests either `--adopt` (declare everything installed,
//!    snapshotting the live selection so the spec round-trips) or a
//!    hand-written starter spec.
//!
//! `niu plugin sync --bootstrap` is the same reconciliation in its quiet
//! startup form (clean machine → zero output), wired into the rc by the
//! setup wizard as a single bootstrap line. Startup installs that fail are
//! memoized and deferred on later startups — only explicit verbs retry.

use std::collections::BTreeMap;

use super::assets;
use super::sources::{
    self, normalize_origin, read_source_registry, SourceInstallRequest, SourceRecord,
};
use super::spec::{self, PluginSpec, SpecSource};

#[derive(Debug, Clone, Default)]
pub struct SyncOptions {
    /// Remove installed-but-undeclared sources (the explicit confirmation
    /// for the cleanup suggestion; never the default).
    pub prune: bool,
    /// Declare installed-but-undeclared sources into the spec (the
    /// imperative → declarative migration): each entry snapshots the live
    /// selection (`enable`/`theme`) so the adopted spec round-trips.
    pub adopt: bool,
    /// The rc startup form (`niu plugin sync --bootstrap`): failed installs
    /// are memoized and not retried on later startups (explicit verbs
    /// retry), so one bad origin can never clone on every terminal.
    pub startup: bool,
    /// A checksum pin handed in by `niu plugin add --checksum` (wt83 #173):
    /// `(target, sha256)` applied to the declared entry whose target (or
    /// resolved origin) matches — the fetch is refused on mismatch, exactly
    /// like `niu plugin source add --checksum`. Every other entry
    /// reconciles unpinned. The pin rides the options (not the spec): the
    /// spec declares WHAT to install; a one-shot supply-chain pin belongs
    /// to the invocation that made it.
    pub checksum_pin: Option<(String, String)>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct SyncRow {
    pub id: String,
    /// installed | awaiting-trust | activated | unchanged | deactivated |
    /// degraded | drift | failed | removed | unsupported | deferred | merged
    pub action: String,
    pub detail: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct SyncReport {
    /// True when a spec file exists (false = legacy imperative mode).
    pub spec_present: bool,
    pub rows: Vec<SyncRow>,
    /// Installed sources the spec does not declare (cleanup suggestions).
    pub undeclared: Vec<String>,
    /// Sources this run declared into the spec: `--adopt` snapshots and
    /// `add`-time adoptions of already-installed sources.
    pub adopted: Vec<String>,
    /// True when nothing needed doing (the bootstrap fast path).
    pub clean: bool,
}

impl SyncReport {
    /// True when the reconciler did (or would do) nothing: no installs, no
    /// materialization changes, no drift, no undeclared sources.
    fn compute_clean(&self) -> bool {
        self.rows.iter().all(|row| row.action == "unchanged") && self.undeclared.is_empty()
    }
}

/// Resolve a spec target the same way `niu plugin add` does: catalog ids
/// expand to their official origin, shorthand/URLs/paths normalize.
fn resolve_spec_origin(target: &str) -> anyhow::Result<(Option<String>, String)> {
    let target = target.trim();
    if target.is_empty() {
        anyhow::bail!("spec entry has an empty target");
    }
    // Catalog id (bare word naming a manager)?
    if !target.contains('/')
        && !target.contains('\\')
        && !target.contains("://")
        && !target.starts_with("./")
        && !target.starts_with("../")
        && !std::path::Path::new(target).is_absolute()
    {
        if let Some(entry) = super::catalog::catalog_entry(target) {
            return Ok((Some(entry.id.to_string()), entry.origin.to_string()));
        }
    }
    Ok((None, normalize_origin(target)))
}

/// The registry record a spec entry owns: explicit id, catalog id, or the
/// recorded origin — in that order.
fn record_for_entry<'a>(
    entry: &SpecSource,
    registry: &'a [SourceRecord],
) -> Option<&'a SourceRecord> {
    if let Some(id) = &entry.id {
        return registry.iter().find(|record| &record.id == id);
    }
    let resolved = resolve_spec_origin(&entry.target).ok();
    // Catalog targets resolve to the manager id.
    if let Some((Some(adapter_id), _)) = &resolved {
        if let Some(record) = registry.iter().find(|record| record.id == *adapter_id) {
            return Some(record);
        }
    }
    // Origin match (covers forks whose derived id differs from the manager
    // id the detection assigned).
    let (_, origin) = resolved?;
    registry
        .iter()
        .find(|record| record.url.trim() == origin.trim())
        .or_else(|| {
            registry
                .iter()
                .find(|record| record.url.trim() == entry.target.trim())
        })
}

/// Would this (target, id, origin) triple already be declared? Returns
/// the existing entry's identifying id/target for the duplicate message.
pub fn declared_entry(
    spec: &PluginSpec,
    target: &str,
    id: Option<&str>,
    origin: &str,
) -> Option<String> {
    declared_entry_state(spec, target, id, origin).map(|(label, _)| label)
}

/// The duplicate check plus the existing entry's install state (niubash#176):
/// the label for the message, and whether a matching source is actually
/// installed. `false` marks a stranded declaration — the spec carries it but
/// no install ever landed, so `niu plugin source remove <label>` fails and
/// `niu plugin sync --prune` is the honest way out.
pub fn declared_entry_state(
    spec: &PluginSpec,
    target: &str,
    id: Option<&str>,
    origin: &str,
) -> Option<(String, bool)> {
    find_declared_entry(spec, target, id, origin).map(|entry| {
        let label = entry.id.clone().unwrap_or_else(|| entry.target.clone());
        let installed = record_for_entry(entry, &read_source_registry()).is_some();
        (label, installed)
    })
}

/// The spec entry a duplicate (target, id, origin) would collide with,
/// matching [`declared_entry`]'s semantics: explicit id first, then the
/// target spelling, then the resolved origin.
fn find_declared_entry<'a>(
    spec: &'a PluginSpec,
    target: &str,
    id: Option<&str>,
    origin: &str,
) -> Option<&'a SpecSource> {
    if let Some(id) = id {
        if let Some(entry) = spec.entry_for_id(id) {
            return Some(entry);
        }
    }
    for entry in &spec.sources {
        if entry.target == target {
            return Some(entry);
        }
        if let Ok((_, entry_origin)) = resolve_spec_origin(&entry.target) {
            if entry_origin == origin || entry_origin == target {
                return Some(entry);
            }
        }
    }
    None
}

/// Remove the declaration(s) matching exactly (target, id) — the `niu
/// plugin add` failure rollback (wt83 #174): a failed add must not leave a
/// spec entry behind that no verb can remove. The add path guarantees the
/// (target, id) pair is undeclared before it appends, so the match is the
/// entry it just added. Returns true when anything was removed.
pub fn remove_declared_entry(target: &str, id: Option<&str>) -> anyhow::Result<bool> {
    let mut spec = spec::load_spec()?.unwrap_or_default();
    let before = spec.sources.len();
    spec.sources
        .retain(|entry| !(entry.target == target && entry.id.as_deref() == id));
    let removed = spec.sources.len() != before;
    if removed {
        spec::save_spec(&spec)?;
    }
    Ok(removed)
}

/// Run the reconciliation. Never prompts (the CLI layer prints; `--prune`
/// is the explicit confirm). See the module docs for the algorithm.
pub fn sync_spec(options: SyncOptions) -> anyhow::Result<SyncReport> {
    // `--adopt` first declares every installed-but-undeclared source (a
    // defensive merge — never clobbers existing entries), then the normal
    // reconciliation runs over the completed spec, so one `sync --adopt`
    // ends in the exact state a plain sync would keep.
    let adopted = if options.adopt {
        adopt_installed_sources()?
    } else {
        Vec::new()
    };
    if let Some(spec) = spec::load_spec()? {
        sync_with_spec(spec, options, adopted)
    } else {
        Ok(SyncReport {
            spec_present: false,
            rows: Vec::new(),
            undeclared: read_source_registry()
                .into_iter()
                .map(|record| record.id)
                .collect(),
            adopted,
            clean: false,
        })
    }
}

/// The imperative → declarative migration (§14.6.3): declare every
/// installed-but-undeclared source into the spec, snapshotting the live
/// selection into `enable` and the active theme into `theme` so the
/// adopted spec round-trips — a plain sync after this changes nothing
/// (byte-stable rc blocks, unchanged registry `spec_enabled`/`spec_theme`).
/// Existing entries are never touched. Returns the ids declared.
pub fn adopt_installed_sources() -> anyhow::Result<Vec<String>> {
    let mut spec = spec::load_spec()?.unwrap_or_default();
    let mut adopted = Vec::new();
    for record in read_source_registry() {
        // Declared already (explicit id, catalog id, or the recorded
        // origin)? Defensive merge: keep the user's entry as written.
        if declared_entry(&spec, &record.url, Some(&record.id), &record.url).is_some() {
            continue;
        }
        let (enable, theme) = assets::live_selection(&record);
        // Kind pin mirrors `niu plugin add`: only per-install managers
        // whose reinstall must not fall back to wild-file detection (bpkg
        // trees) carry one; catalog/manager ids and wild file sources
        // resolve their adapter from the pinned id or detection.
        let kind = sources::adapter_for(&record.adapter)
            .filter(|adapter| adapter.per_install_id() && adapter.id() != "file")
            .map(|_| record.adapter.clone());
        spec.sources.push(SpecSource {
            target: record.url.clone(),
            id: Some(record.id.clone()),
            kind,
            ref_name: None,
            theme,
            enable,
        });
        adopted.push(record.id.clone());
    }
    if !adopted.is_empty() {
        spec::save_spec(&spec)?;
    }
    Ok(adopted)
}

/// A declared-but-never-installed entry whose last startup install attempt
/// failed — the memoized `deferred` population (wt83 #174): a legacy
/// failed `niu plugin add` left it stranded, no verb could remove it, and
/// every startup printed the deferred line. `--prune` is its way out.
struct StrandedEntry {
    label: String,
    target: String,
    id: Option<String>,
    ref_name: Option<String>,
    origin: String,
}

fn sync_with_spec(
    mut spec: PluginSpec,
    options: SyncOptions,
    adopted: Vec<String>,
) -> anyhow::Result<SyncReport> {
    let mut registry = read_source_registry();
    let mut rows: Vec<SyncRow> = Vec::new();
    let mut spec_changed = false;
    let mut adopted = adopted;
    let mut declared_ids: Vec<String> = Vec::new();
    // Entries that resolved to a source an earlier entry already claimed
    // (the same tree declared under two spellings); dropped after the pass.
    let mut merged: Vec<usize> = Vec::new();
    // Snapshot the stranded declarations BEFORE the pass — a non-startup
    // retry clears the failure memo on the way in, and the memo is what
    // marks the entry as stranded rather than merely not-yet-synced.
    let mut stranded: Vec<StrandedEntry> = Vec::new();
    for entry in spec.sources.iter() {
        if record_for_entry(entry, &registry).is_some() {
            continue;
        }
        let Ok((_, origin)) = resolve_spec_origin(&entry.target) else {
            continue;
        };
        if bootstrap_failure_recorded(&origin, &entry.ref_name) {
            stranded.push(StrandedEntry {
                label: entry.id.clone().unwrap_or_else(|| entry.target.clone()),
                target: entry.target.clone(),
                id: entry.id.clone(),
                ref_name: entry.ref_name.clone(),
                origin,
            });
        }
    }

    // Theme-claim reconciliation (niubash#168) runs BEFORE materialization:
    // a theme pick written to the rc + spec is the user's latest expressed
    // choice, so a stale same-name claim must never re-materialize the old
    // framework's block over it. Reconciliation happens here, toward the
    // rc's live state — never by flipping a working theme block.
    for reconciliation in reconcile_theme_claims(&mut spec, &registry) {
        spec_changed = true;
        rows.push(SyncRow {
            id: reconciliation.id,
            action: "reconciled".to_string(),
            detail: reconciliation.detail,
        });
    }

    // The niubash#168 floor-yield input, per entry: does any OTHER entry
    // claim a theme? A selection-less source then leaves its framework
    // floor dormant instead of loading it on top of the claimed theme.
    let theme_claimed_elsewhere: Vec<bool> = spec
        .sources
        .iter()
        .enumerate()
        .map(|(index, _)| {
            spec.sources.iter().enumerate().any(|(other, other_entry)| {
                other != index
                    && other_entry
                        .theme
                        .as_deref()
                        .is_some_and(|theme| !theme.is_empty())
            })
        })
        .collect();

    for (entry_index, entry) in spec.sources.iter_mut().enumerate() {
        let (catalog_hint, origin) = match resolve_spec_origin(&entry.target) {
            Ok(resolved) => resolved,
            Err(err) => {
                rows.push(SyncRow {
                    id: entry.id.clone().unwrap_or_else(|| entry.target.clone()),
                    action: "failed".to_string(),
                    detail: err.to_string(),
                });
                continue;
            }
        };
        // An explicit `kind` pin (e.g. a tree adopted as `bpkg`) wins over
        // catalog resolution and over auto-detection: later syncs reinstall
        // through that adapter and refuse when the fingerprint stops
        // matching, instead of silently degrading to wild-file loading.
        let adapter_hint = entry.kind.clone().or(catalog_hint);
        let existing = record_for_entry(entry, &registry);
        let record = match existing {
            Some(record) => record.clone(),
            None => {
                // Startup guard (1.3.1): a declared-but-missing source whose
                // last startup install attempt failed is not retried here —
                // one memoized line, no fetch churn. Explicit verbs (a plain
                // `niu plugin sync`, `niu plugin add`) retry and clear the
                // memo on the way in.
                if options.startup && bootstrap_failure_recorded(&origin, &entry.ref_name) {
                    rows.push(SyncRow {
                        id: entry.id.clone().unwrap_or_else(|| entry.target.clone()),
                        action: "deferred".to_string(),
                        detail: "install not retried at startup (last attempt failed); \
                                 `niu plugin sync` retries"
                            .to_string(),
                    });
                    continue;
                }
                // Declared but not installed: fetch (fetch gate only —
                // trust is never automatic). An install identity that is
                // already registered is ADOPTED, not refused — the spec
                // declares what exists.
                if !options.startup {
                    clear_bootstrap_failure(&origin, &entry.ref_name);
                }
                // The `niu plugin add --checksum` hand-off (wt83 #173): the
                // pin applies to the entry whose target (or resolved
                // origin) it names — a one-entry scope, so other pending
                // declarations are never pinned by someone else's flag.
                let expected_checksum = options
                    .checksum_pin
                    .as_ref()
                    .filter(|(target, _)| target == &entry.target || target == &origin)
                    .map(|(_, checksum)| checksum.clone());
                let request = SourceInstallRequest {
                    adapter: adapter_hint.clone(),
                    origin: origin.clone(),
                    ref_name: entry.ref_name.clone(),
                    commit: None,
                    expected_checksum,
                    id: entry.id.clone(),
                    entry: None,
                    // Startup fetches run under a hard wall-clock budget
                    // (sources::STARTUP_FETCH_BUDGET): a bad network day
                    // (TLS resets, blackholes) must cost the interactive
                    // session one bounded attempt, not an unbounded hang;
                    // the memo then defers later startups, and the explicit
                    // verbs below retry without the cap.
                    fetch_budget: options.startup.then_some(sources::startup_fetch_budget()),
                };
                match sources::install_or_adopt(request) {
                    Ok((record, already_installed)) => {
                        // Persist the derived id into the spec so later
                        // syncs match directly instead of re-deriving.
                        if entry.id.is_none() {
                            entry.id = Some(record.id.clone());
                            spec_changed = true;
                        }
                        if already_installed {
                            adopted.push(record.id.clone());
                        }
                        if !record.trusted {
                            rows.push(SyncRow {
                                id: record.id.clone(),
                                action: "awaiting-trust".to_string(),
                                detail: if already_installed {
                                    format!(
                                        "already installed (untrusted) — review, then \
                                         `niu plugin trust {}`",
                                        record.id
                                    )
                                } else {
                                    format!(
                                        "installed {} (untrusted) — review, then \
                                         `niu plugin trust {}`",
                                        record.version, record.id
                                    )
                                },
                            });
                        }
                        registry = read_source_registry();
                        record
                    }
                    Err(err) => {
                        if options.startup {
                            record_bootstrap_failure(&origin, &entry.ref_name, &err.to_string());
                        }
                        rows.push(SyncRow {
                            id: entry.id.clone().unwrap_or_else(|| entry.target.clone()),
                            action: "failed".to_string(),
                            detail: err.to_string(),
                        });
                        continue;
                    }
                }
            }
        };
        // One source, two declarations (the same tree added under two
        // spellings, e.g. the catalog id and a path): keep the FIRST
        // declaration, drop the rest — diverging `enable` lists on one
        // source would otherwise fight and flip the rc on every sync.
        if declared_ids.contains(&record.id) {
            rows.push(SyncRow {
                id: record.id.clone(),
                action: "merged".to_string(),
                detail: "duplicate declaration dropped (this source is already \
                         declared above)"
                    .to_string(),
            });
            merged.push(entry_index);
            continue;
        }
        declared_ids.push(record.id.clone());

        if !record.trusted {
            if !rows.iter().any(|row| row.id == record.id) {
                rows.push(SyncRow {
                    id: record.id.clone(),
                    action: "awaiting-trust".to_string(),
                    detail: format!(
                        "installed but untrusted — review, then `niu plugin trust {}`",
                        record.id
                    ),
                });
            }
            continue;
        }
        if !record.path.is_dir() {
            rows.push(SyncRow {
                id: record.id.clone(),
                action: "degraded".to_string(),
                detail: format!(
                    "tree missing — repair with `niu plugin restore {}` (native fallback active)",
                    record.id
                ),
            });
            continue;
        }

        // Materialize the spec selection (idempotent; hand-added entries
        // preserved) and persist the materialized state into the lock.
        match assets::materialize_spec_selection(
            &record,
            entry,
            theme_claimed_elsewhere
                .get(entry_index)
                .copied()
                .unwrap_or(false),
        ) {
            Ok(materialized) => {
                let mut updated = record.clone();
                updated.spec_enabled = Some(materialized.spec_enabled.unwrap_or_default());
                updated.spec_theme = materialized.spec_theme;
                if updated.spec_enabled != record.spec_enabled
                    || updated.spec_theme != record.spec_theme
                {
                    write_record(&mut registry, updated);
                }
                let action = materialized.action;
                rows.push(SyncRow {
                    id: record.id.clone(),
                    action,
                    detail: materialized.detail,
                });
            }
            Err(err) => rows.push(SyncRow {
                id: record.id.clone(),
                action: "failed".to_string(),
                detail: err.to_string(),
            }),
        }
    }
    if spec_changed || !merged.is_empty() {
        for index in merged.into_iter().rev() {
            spec.sources.remove(index);
        }
        spec::save_spec(&spec)?;
    }

    // Installed but not declared: suggest (or, with --prune, remove).
    let mut undeclared: Vec<String> = Vec::new();
    for record in read_source_registry() {
        if declared_ids.contains(&record.id) {
            continue;
        }
        undeclared.push(record.id.clone());
        if record.spec_enabled.is_some() {
            // The spec owned this source and no longer declares it: drop
            // its managed block (spec is the truth) but keep the tree. A
            // failed removal (a malformed block) is a failed row, never a
            // "deactivated" print over a block that still loads (wt83 #175).
            match remove_activation_block(&record) {
                Ok(()) => {
                    let mut registry_now = read_source_registry();
                    if let Some(stored) = registry_now
                        .iter_mut()
                        .find(|candidate| candidate.id == record.id)
                    {
                        stored.spec_enabled = None;
                        stored.spec_theme = None;
                        sources::write_source_registry(&registry_now)?;
                    }
                    rows.push(SyncRow {
                        id: record.id.clone(),
                        action: "deactivated".to_string(),
                        detail: format!(
                            "no longer declared — activation dropped; tree kept \
                             (`niu plugin source remove {id}` to delete)",
                            id = record.id
                        ),
                    });
                }
                Err(err) => rows.push(SyncRow {
                    id: record.id.clone(),
                    action: "failed".to_string(),
                    detail: err.to_string(),
                }),
            }
        }
    }
    let mut pruned: Vec<SyncRow> = Vec::new();
    if options.prune {
        for id in &undeclared {
            match sources::remove_source(id) {
                Ok(path) => pruned.push(SyncRow {
                    id: id.clone(),
                    action: "removed".to_string(),
                    detail: format!("pruned (tree {} deleted)", path.display()),
                }),
                Err(err) => pruned.push(SyncRow {
                    id: id.clone(),
                    action: "failed".to_string(),
                    detail: err.to_string(),
                }),
            }
        }
        undeclared.clear();

        // Stranded-declaration cleanup (--prune is the explicit confirm;
        // wt83 #174): the snapshotted entries that are STILL not installed
        // after this pass come out of the spec with a printed row. One
        // that this pass installed is a healthy declaration again — kept.
        for entry in stranded {
            let probe = SpecSource {
                target: entry.target.clone(),
                id: entry.id.clone(),
                kind: None,
                ref_name: entry.ref_name.clone(),
                theme: None,
                enable: Vec::new(),
            };
            let fresh = read_source_registry();
            if record_for_entry(&probe, &fresh).is_some() {
                continue;
            }
            match remove_declared_entry(&entry.target, entry.id.as_deref()) {
                Ok(true) => {
                    clear_bootstrap_failure(&entry.origin, &entry.ref_name);
                    pruned.push(SyncRow {
                        id: entry.label,
                        action: "removed".to_string(),
                        detail: "stranded declaration pruned — never installed \
                                 and its startup install kept failing"
                            .to_string(),
                    });
                }
                Ok(false) => {}
                Err(err) => pruned.push(SyncRow {
                    id: entry.label,
                    action: "failed".to_string(),
                    detail: err.to_string(),
                }),
            }
        }
    }
    rows.extend(pruned);

    let mut report = SyncReport {
        spec_present: true,
        rows,
        undeclared,
        adopted,
        clean: false,
    };
    report.clean = report.compute_clean();
    Ok(report)
}

// ── Theme-claim reconciliation (niubash#168) ─────────────────────────────────
//
// The same theme name can exist in several frameworks (powerbash10k ships
// in oh-my-bash AND bash-it). When the spec disagrees about WHICH source
// owns a theme — two entries claiming it, or one entry claiming it while
// another source's rc block already displays it — the claim is resolved to
// exactly one owner and every loser is reconciled toward the rc's live
// state: the winner keeps (or is) the working block, a loser's claim is
// cleared, and a loser that declared the theme and nothing else has its
// declaration removed so the next pass drops its activation. Sync never
// re-routes a working theme block; it reports what it reconciled.

struct ThemeReconciliation {
    id: String,
    detail: String,
}

/// One spec entry's standing in the theme-ownership picture.
struct ThemeClaimMember {
    /// Index into `spec.sources`.
    index: usize,
    /// The entry's identifying id/target (for the reconciliation row).
    id: String,
    /// The registry record id (for the framework priority ranking).
    record_id: String,
    /// The theme the record's managed rc block currently carries.
    live_theme: Option<String>,
    /// The entry's non-empty spec claim, if any (`theme = ''` is an
    /// explicit "no theme here" and never joins a group).
    claim: Option<String>,
}

fn reconcile_theme_claims(
    spec: &mut PluginSpec,
    registry: &[SourceRecord],
) -> Vec<ThemeReconciliation> {
    let mut members: Vec<ThemeClaimMember> = Vec::new();
    for (index, entry) in spec.sources.iter().enumerate() {
        let Some(record) = record_for_entry(entry, registry) else {
            continue;
        };
        // The same gate materialization applies: only a trusted, healthy
        // source has a meaningful live state.
        if !record.trusted || !record.path.is_dir() {
            continue;
        }
        let live_theme = assets::live_managed_theme(record);
        members.push(ThemeClaimMember {
            index,
            id: entry.id.clone().unwrap_or_else(|| entry.target.clone()),
            record_id: record.id.clone(),
            live_theme,
            claim: entry.theme.clone().filter(|theme| !theme.is_empty()),
        });
    }
    // Group the members by theme name: every claimant, plus entries with no
    // claim whose live block already displays the name (the rc's live
    // expression of a pick — e.g. the wizard's write — joins the group it
    // contradicts).
    let mut groups: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for (member_index, member) in members.iter().enumerate() {
        if let Some(theme) = &member.claim {
            groups.entry(theme.clone()).or_default().push(member_index);
        } else if member.claim.is_none() {
            if let Some(live) = &member.live_theme {
                groups.entry(live.clone()).or_default().push(member_index);
            }
        }
    }
    let mut out = Vec::new();
    let mut dropped: Vec<usize> = Vec::new();
    for (theme, group) in groups {
        if group.len() < 2 {
            continue;
        }
        // The owner: rc live evidence first, then an explicit spec claim
        // over a bare live block, then the framework priority
        // (`sources::primary_theme_source_rank` — oh-my-bash, the primary
        // external framework, matching the wt61 G2 gallery rank), then the
        // spec's declaration order.
        let mut ranked = group;
        ranked.sort_by(|&a, &b| {
            let key = |m: usize| (members[m].live_theme.is_some(), members[m].claim.is_some());
            key(b)
                .cmp(&key(a))
                .then_with(|| {
                    sources::primary_theme_source_rank(&members[a].record_id)
                        .cmp(&sources::primary_theme_source_rank(&members[b].record_id))
                })
                .then_with(|| a.cmp(&b))
        });
        let winner = &members[ranked[0]];
        for &member_index in &ranked[1..] {
            let member = &members[member_index];
            // Live-joiners claim nothing — there is nothing to reconcile.
            let Some(_claim) = &member.claim else {
                continue;
            };
            let entry = &mut spec.sources[member.index];
            entry.theme = Some(String::new());
            let removed = entry.enable.is_empty();
            let mut detail = format!(
                "theme '{theme}' is owned by {} — stale claim cleared (the rc's live \
                 state and the latest user pick win)",
                winner.record_id
            );
            if removed {
                dropped.push(member.index);
                detail.push_str("; declaration removed (it carried the theme only)");
            }
            out.push(ThemeReconciliation {
                id: member.id.clone(),
                detail,
            });
        }
    }
    for index in dropped.into_iter().rev() {
        spec.sources.remove(index);
    }
    out
}

fn write_record(registry: &mut Vec<SourceRecord>, updated: SourceRecord) {
    match registry.iter_mut().find(|record| record.id == updated.id) {
        Some(slot) => *slot = updated,
        None => registry.push(updated),
    }
    if let Err(err) = sources::write_source_registry(registry) {
        log::warn!("failed to persist spec state: {err}");
    }
}

/// Drop a source's managed rc activation (and bash-it enabled/ entries)
/// without touching the rest of the tree. Errors surface (wt83 #175): a
/// removal that did not happen must reach the report, not vanish.
fn remove_activation_block(record: &SourceRecord) -> anyhow::Result<()> {
    assets::deactivate_block(record)
}

/// `niu plugin update` (no id): the vim-plug `:PlugUpdate` move — refetch
/// every git-origin source at its recorded ref's tip (the pre-14.6
/// meaning of bare `niu plugin sync`).
pub fn update_all_to_ref_tip() -> Vec<sources::SourceSyncOutcome> {
    sources::sync_sources()
}

// ── Startup install-failure memo (1.3.1) ─────────────────────────────────────
//
// The rc's bootstrap line runs on every terminal. When a declared source
// cannot install (network offline, a moved origin, a fingerprint the
// adapter rejects), retrying the full fetch each startup is unbounded
// churn — the 1.3.0 dead end cloned bash-it on every new terminal. The
// memo records (target, ref) → error at startup; later startups defer the
// retry with one line; the explicit verbs (`niu plugin sync` / `add`)
// clear the memo and attempt again.

const BOOTSTRAP_FAILURE_SCHEMA: &str = "niubash:plugin-bootstrap-failures@1";

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct BootstrapFailure {
    /// Resolved install origin of the spec entry.
    target: String,
    /// The entry's `ref`, when it declared one.
    #[serde(rename = "ref", default, skip_serializing_if = "Option::is_none")]
    ref_name: Option<String>,
    pub error: String,
    #[serde(default)]
    pub at: String,
}

fn bootstrap_failure_path() -> std::path::PathBuf {
    sources::sources_root().join("bootstrap-failures.toml")
}

fn read_bootstrap_failures() -> Vec<BootstrapFailure> {
    let Ok(text) = std::fs::read_to_string(bootstrap_failure_path()) else {
        return Vec::new();
    };
    #[derive(serde::Deserialize)]
    struct Ledger {
        #[serde(default)]
        failure: Vec<BootstrapFailure>,
    }
    toml::from_str::<Ledger>(&text)
        .map(|ledger| ledger.failure)
        .unwrap_or_default()
}

fn write_bootstrap_failures(failures: &[BootstrapFailure]) {
    let path = bootstrap_failure_path();
    let Some(parent) = path.parent() else {
        return;
    };
    if std::fs::create_dir_all(parent).is_err() {
        return;
    }
    let mut body = format!("# Startup install-failure memo (niu plugin sync --bootstrap).\n# Cleared by an explicit `niu plugin sync` / `niu plugin add` retry.\nschema = \"{BOOTSTRAP_FAILURE_SCHEMA}\"\n");
    for failure in failures {
        let quote =
            |value: &str| format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""));
        body.push_str(&format!(
            "\n[[failure]]\ntarget = {}\n",
            quote(&failure.target)
        ));
        if let Some(ref_name) = &failure.ref_name {
            body.push_str(&format!("ref = {}\n", quote(ref_name)));
        }
        body.push_str(&format!(
            "error = {}\nat = {}\n",
            quote(&failure.error),
            quote(&failure.at)
        ));
    }
    let _ = std::fs::write(path, body);
}

fn failure_key(target: &str, ref_name: &Option<String>) -> (String, Option<String>) {
    (target.trim().to_string(), ref_name.clone())
}

/// True when the last startup attempt for this (target, ref) failed.
fn bootstrap_failure_recorded(target: &str, ref_name: &Option<String>) -> bool {
    let key = failure_key(target, ref_name);
    read_bootstrap_failures()
        .iter()
        .any(|failure| failure_key(&failure.target, &failure.ref_name) == key)
}

/// Memo a startup install failure (upsert by target+ref).
fn record_bootstrap_failure(target: &str, ref_name: &Option<String>, error: &str) {
    let key = failure_key(target, ref_name);
    let mut failures = read_bootstrap_failures();
    match failures
        .iter_mut()
        .find(|failure| failure_key(&failure.target, &failure.ref_name) == key)
    {
        Some(slot) => {
            slot.error = error.to_string();
            slot.at = sources::now_timestamp();
        }
        None => failures.push(BootstrapFailure {
            target: target.trim().to_string(),
            ref_name: ref_name.clone(),
            error: error.to_string(),
            at: sources::now_timestamp(),
        }),
    }
    write_bootstrap_failures(&failures);
}

/// Clear the memo for one (target, ref): the explicit retry is happening.
fn clear_bootstrap_failure(target: &str, ref_name: &Option<String>) {
    let key = failure_key(target, ref_name);
    let mut failures = read_bootstrap_failures();
    let before = failures.len();
    failures.retain(|failure| failure_key(&failure.target, &failure.ref_name) != key);
    if failures.len() != before {
        write_bootstrap_failures(&failures);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::PROCESS_STATE_LOCK;
    use std::fs;
    use std::path::{Path, PathBuf};

    struct EnvGuard {
        name: &'static str,
        previous: Option<std::ffi::OsString>,
    }

    impl EnvGuard {
        fn set(name: &'static str, value: &str) -> Self {
            let previous = std::env::var_os(name);
            std::env::set_var(name, value);
            Self { name, previous }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match &self.previous {
                Some(value) => std::env::set_var(self.name, value),
                None => std::env::remove_var(self.name),
            }
        }
    }

    fn unique_temp_dir(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "niu-sync-{}-{}-{}",
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

    struct Sandbox {
        _home: EnvGuard,
        _userprofile: EnvGuard,
        _sources: EnvGuard,
        _spec: EnvGuard,
        temp: PathBuf,
    }

    fn sandbox(label: &str) -> Sandbox {
        let temp = unique_temp_dir(label);
        let home = temp.join("home");
        fs::create_dir_all(&home).unwrap();
        Sandbox {
            _home: EnvGuard::set("HOME", &home.to_string_lossy()),
            _userprofile: EnvGuard::set("USERPROFILE", &home.to_string_lossy()),
            _sources: EnvGuard::set(
                "NIU_PLUGIN_SOURCES_ROOT",
                &temp.join("sources").to_string_lossy(),
            ),
            _spec: EnvGuard::set(
                "NIU_PLUGIN_SPEC",
                &home.join(".niubash/plugins.toml").to_string_lossy(),
            ),
            temp,
        }
    }

    fn write_omb_fixture(root: &Path) {
        fs::create_dir_all(root.join("themes/agnoster")).unwrap();
        fs::create_dir_all(root.join("plugins/git")).unwrap();
        fs::create_dir_all(root.join("plugins/npm")).unwrap();
        fs::write(
            root.join("oh-my-bash.sh"),
            "#!/usr/bin/env bash\ncase $- in *i*) ;; *) return;; esac\n",
        )
        .unwrap();
        fs::write(
            root.join("themes/agnoster/agnoster.theme.sh"),
            "PS1='agnoster> '\n",
        )
        .unwrap();
        fs::write(
            root.join("plugins/git/git.plugin.sh"),
            "alias gg='git status'\n",
        )
        .unwrap();
        fs::write(
            root.join("plugins/npm/npm.plugin.sh"),
            "alias ni='npm install'\n",
        )
        .unwrap();
    }

    fn write_wild_fixture(root: &Path) {
        fs::write(root.join("pre.sh"), "pre_fun() { echo pre; }\n").unwrap();
    }

    /// The niubash#168 shape: the SAME theme name shipped by BOTH frameworks
    /// (powerbash10k lives in oh-my-bash and bash-it; the fixtures use
    /// `demox`, the journey's shared name).
    fn write_bash_it_shared_theme_fixture(root: &Path) {
        fs::create_dir_all(root.join("lib")).unwrap();
        fs::create_dir_all(root.join("themes/demox")).unwrap();
        fs::write(
            root.join("bash_it.sh"),
            "#!/usr/bin/env bash\nfor _f in \"$BASH_IT/enabled\"/*.bash; do [ -r \"$_f\" ] && . \"$_f\"; done\nunset _f\n",
        )
        .unwrap();
        fs::write(root.join("lib/composure.bash"), "# composure\n").unwrap();
        fs::write(
            root.join("themes/demox/demox.theme.bash"),
            "PS1='demox-bit> '\n",
        )
        .unwrap();
    }

    fn write_omb_shared_theme_fixture(root: &Path) {
        fs::create_dir_all(root.join("themes/demox")).unwrap();
        fs::write(
            root.join("oh-my-bash.sh"),
            "#!/usr/bin/env bash\ncase $- in *i*) ;; *) return;; esac\n",
        )
        .unwrap();
        fs::write(
            root.join("themes/demox/demox.theme.sh"),
            "omb_theme_demox() { PS1='demox-omb> '; }\nomb_theme_demox\n",
        )
        .unwrap();
    }

    fn rc_text() -> String {
        fs::read_to_string(assets::rc_file()).unwrap_or_default()
    }

    fn spec_sources() -> Vec<SpecSource> {
        spec::load_spec().unwrap().unwrap_or_default().sources
    }

    #[test]
    fn sync_installs_declared_sources_and_materializes_idempotently() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let holder = unique_temp_dir("omb-holder");
        let origin = holder.join("oh-my-fixture");
        fs::create_dir_all(&origin).unwrap();
        write_omb_fixture(&origin);
        let box_ = sandbox("omb-idempotent");

        // Hand-written spec: declare the local tree with two plugins and a
        // theme. Target is a path (offline), ref left out.
        spec::save_spec(&PluginSpec {
            schema: Some(spec::PLUGIN_SPEC_SCHEMA.to_string()),
            sources: vec![SpecSource {
                target: origin.to_string_lossy().into_owned(),
                id: Some("oh-my-bash".to_string()),
                kind: None,
                ref_name: None,
                theme: Some("agnoster".to_string()),
                enable: vec!["git".to_string(), "npm".to_string()],
            }],
        })
        .unwrap();

        // Sync 1: installs (fetch gate), lands untrusted.
        let report = sync_spec(SyncOptions::default()).unwrap();
        assert_eq!(report.rows.len(), 1, "{:?}", report.rows);
        assert_eq!(report.rows[0].action, "awaiting-trust", "{:?}", report.rows);
        assert!(
            report.rows[0].detail.contains("niu plugin trust"),
            "{:?}",
            report.rows
        );
        assert!(rc_text().is_empty(), "untrusted sources activate nothing");

        sources::trust_source("oh-my-bash").unwrap();

        // Sync 2: materializes the spec into the managed block.
        let report = sync_spec(SyncOptions::default()).unwrap();
        assert_eq!(report.rows[0].action, "activated", "{:?}", report.rows);
        let rc = rc_text();
        assert!(rc.contains("OSH_THEME='agnoster'"), "{rc}");
        assert!(rc.contains("plugins=('git' 'npm')"), "{rc}");
        assert!(!report.clean, "{:?}", report);

        // Sync 3 (idempotency, pinned by integration too): byte-identical
        // rc, unchanged rows, clean report.
        let before = rc_text();
        let report = sync_spec(SyncOptions::default()).unwrap();
        assert_eq!(report.rows[0].action, "unchanged", "{:?}", report.rows);
        assert_eq!(rc_text(), before, "rc must be byte-identical");
        assert!(report.clean, "{:?}", report);

        // Removing an entry from the spec drops it from the rc block, and
        // hand-added entries survive: simulate a hand edit by adding 'g2'
        // directly to the rc array, then syncing after removing npm.
        let hand = before.replace("plugins=('git' 'npm')", "plugins=('git' 'npm' 'g2')");
        fs::write(assets::rc_file(), &hand).unwrap();
        let mut next_spec = spec::load_spec().unwrap().unwrap();
        next_spec.sources[0].enable.retain(|name| name != "npm");
        spec::save_spec(&next_spec).unwrap();
        let report = sync_spec(SyncOptions::default()).unwrap();
        assert_eq!(report.rows[0].action, "activated", "{:?}", report.rows);
        let rc = rc_text();
        assert!(
            rc.contains("plugins=('git' 'g2')"),
            "spec-removed npm is dropped, hand-added g2 survives: {rc}"
        );
        assert!(rc.contains("OSH_THEME='agnoster'"), "{rc}");

        let _ = fs::remove_dir_all(&holder);
        let _ = fs::remove_dir_all(&box_.temp);
    }

    #[test]
    fn sync_undeclared_sources_are_suggested_then_pruned() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let holder = unique_temp_dir("prune-holder");
        let origin = holder.join("wildy");
        fs::create_dir_all(&origin).unwrap();
        write_wild_fixture(&origin);
        let box_ = sandbox("prune");

        // Imperative install (no spec): legacy mode reports undeclared.
        sources::add_source(SourceInstallRequest {
            adapter: None,
            origin: origin.to_string_lossy().into_owned(),
            ref_name: None,
            commit: None,
            expected_checksum: None,
            id: None,
            entry: None,
            // Explicit (non-startup) install: no fetch budget (wt71).
            fetch_budget: None,
        })
        .unwrap();
        let report = sync_spec(SyncOptions::default()).unwrap();
        assert!(!report.spec_present, "{:?}", report);
        assert_eq!(report.undeclared, ["wildy"], "{:?}", report);

        // An empty spec exists → same suggestion, no auto-delete.
        spec::save_spec(&PluginSpec::default()).unwrap();
        let report = sync_spec(SyncOptions::default()).unwrap();
        assert!(report.spec_present, "{:?}", report);
        assert_eq!(report.undeclared, ["wildy"], "{:?}", report);
        assert!(
            box_.temp.join("sources/wildy/pre.sh").is_file(),
            "undeclared trees are never auto-deleted"
        );

        // --prune is the explicit confirm.
        let report = sync_spec(SyncOptions {
            prune: true,
            ..SyncOptions::default()
        })
        .unwrap();
        assert!(report.undeclared.is_empty(), "{:?}", report);
        assert!(
            report
                .rows
                .iter()
                .any(|row| row.id == "wildy" && row.action == "removed"),
            "{:?}",
            report.rows
        );
        assert!(!box_.temp.join("sources/wildy").exists());
        assert!(read_source_registry().is_empty());

        let _ = fs::remove_dir_all(&holder);
        let _ = fs::remove_dir_all(&box_.temp);
    }

    #[test]
    fn spec_declared_source_removal_drops_the_block_and_keeps_the_tree() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let holder = unique_temp_dir("drop-holder");
        let origin = holder.join("oh-my-fixture");
        fs::create_dir_all(&origin).unwrap();
        write_omb_fixture(&origin);
        let box_ = sandbox("drop");

        spec::save_spec(&PluginSpec {
            schema: None,
            sources: vec![SpecSource {
                target: origin.to_string_lossy().into_owned(),
                id: Some("oh-my-bash".to_string()),
                kind: None,
                ref_name: None,
                theme: None,
                enable: vec!["git".to_string()],
            }],
        })
        .unwrap();
        sync_spec(SyncOptions::default()).unwrap();
        sources::trust_source("oh-my-bash").unwrap();
        sync_spec(SyncOptions::default()).unwrap();
        assert!(rc_text().contains("plugins=('git')"), "{}", rc_text());

        // Remove the declaration → sync drops the activation, keeps tree.
        spec::save_spec(&PluginSpec::default()).unwrap();
        let report = sync_spec(SyncOptions::default()).unwrap();
        assert!(
            report
                .rows
                .iter()
                .any(|row| row.id == "oh-my-bash" && row.action == "deactivated"),
            "{:?}",
            report.rows
        );
        assert!(!rc_text().contains("plugins="), "{}", rc_text());
        assert!(
            box_.temp.join("sources/oh-my-bash/oh-my-bash.sh").is_file(),
            "tree kept"
        );

        let _ = fs::remove_dir_all(&holder);
        let _ = fs::remove_dir_all(&box_.temp);
    }

    #[test]
    fn sync_binds_derived_ids_back_into_the_spec() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let holder = unique_temp_dir("bind-holder");
        let origin = holder.join("prelib");
        fs::create_dir_all(&origin).unwrap();
        write_wild_fixture(&origin);
        let box_ = sandbox("bind");

        // No explicit id: the install derives one; sync persists it.
        spec::save_spec(&PluginSpec {
            schema: None,
            sources: vec![SpecSource {
                target: origin.to_string_lossy().into_owned(),
                id: None,
                kind: None,
                ref_name: None,
                theme: None,
                enable: vec!["pre.sh".to_string()],
            }],
        })
        .unwrap();
        let report = sync_spec(SyncOptions::default()).unwrap();
        assert_eq!(report.rows[0].id, "prelib", "{:?}", report.rows);
        let sources = spec_sources();
        assert_eq!(sources[0].id.as_deref(), Some("prelib"), "{sources:?}");

        // A second sync matches the bound id (no reinstall attempt).
        let report = sync_spec(SyncOptions::default()).unwrap();
        assert!(
            report.rows.iter().all(|row| row.action != "failed"),
            "{:?}",
            report.rows
        );
        assert_eq!(read_source_registry().len(), 1);

        let _ = fs::remove_dir_all(&holder);
        let _ = fs::remove_dir_all(&box_.temp);
    }

    #[test]
    fn missing_spec_reports_legacy_mode() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let box_ = sandbox("legacy");
        let report = sync_spec(SyncOptions::default()).unwrap();
        assert!(!report.spec_present);
        assert!(report.rows.is_empty());
        let _ = fs::remove_dir_all(&box_.temp);
    }

    /// The 1.3.1 adoption contract (F2): `--adopt` declares installed
    /// sources by snapshotting the LIVE selection (the wizard's theme block
    /// plus whatever is enabled) — the adopted spec round-trips, meaning a
    /// plain sync afterwards is a byte-stable no-op.
    #[test]
    fn adopt_snapshots_live_selection_and_round_trips() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let holder = unique_temp_dir("adopt-holder");
        let origin = holder.join("oh-my-fixture");
        fs::create_dir_all(&origin).unwrap();
        write_omb_fixture(&origin);
        let box_ = sandbox("adopt");

        // The wizard-shaped imperative state: installed + trusted + a live
        // managed block (theme pick + one enabled plugin), NO spec.
        sources::add_source(SourceInstallRequest {
            origin: origin.to_string_lossy().into_owned(),
            ..Default::default()
        })
        .unwrap();
        sources::trust_source("oh-my-bash").unwrap();
        let block = assets::build_theme_block("oh-my-bash", "agnoster").expect("theme block");
        let with_plugin = block.replacen(
            "OSH_THEME='agnoster'\n",
            "OSH_THEME='agnoster'\nplugins=('git')\n",
            1,
        );
        fs::write(assets::rc_file(), &with_plugin).unwrap();

        // --adopt declares it with the snapshot.
        let report = sync_spec(SyncOptions {
            adopt: true,
            ..SyncOptions::default()
        })
        .unwrap();
        assert_eq!(report.adopted, ["oh-my-bash"], "{:?}", report);
        assert!(report.spec_present, "{:?}", report);
        assert!(report.undeclared.is_empty(), "{:?}", report);
        let declared = spec_sources();
        assert_eq!(declared.len(), 1, "{declared:?}");
        assert_eq!(
            declared[0].target,
            origin.to_string_lossy(),
            "the recorded origin is the target"
        );
        assert_eq!(declared[0].id.as_deref(), Some("oh-my-bash"));
        assert_eq!(declared[0].theme.as_deref(), Some("agnoster"));
        assert_eq!(declared[0].enable, ["git"], "{declared:?}");
        // The adopt run itself materializes without disturbing the block.
        assert_eq!(rc_text(), with_plugin, "the live block is byte-stable");

        // Round-trip: a plain sync is a no-op — clean report, byte-identical
        // rc, spec, and registry (spec_enabled/spec_theme included).
        let spec_text = fs::read_to_string(spec::spec_path()).unwrap();
        let registry_text =
            fs::read_to_string(sources::sources_root().join("registry.toml")).unwrap();
        let report = sync_spec(SyncOptions::default()).unwrap();
        assert!(report.clean, "{:?}", report);
        assert_eq!(rc_text(), with_plugin, "rc byte-identical after plain sync");
        assert_eq!(
            fs::read_to_string(spec::spec_path()).unwrap(),
            spec_text,
            "spec byte-identical after plain sync"
        );
        assert_eq!(
            fs::read_to_string(sources::sources_root().join("registry.toml")).unwrap(),
            registry_text,
            "registry byte-identical after plain sync"
        );

        let _ = fs::remove_dir_all(&holder);
        let _ = fs::remove_dir_all(&box_.temp);
    }

    /// Untrusted imperative installs (the 1.3.0 wizard's bash-completion
    /// state) adopt too: the declaration lands, the trust gate stays
    /// visible, and plain syncs churn nothing.
    #[test]
    fn adopt_declares_untrusted_sources_without_activating() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let holder = unique_temp_dir("adopt-untrusted");
        let origin = holder.join("wildy");
        fs::create_dir_all(&origin).unwrap();
        write_wild_fixture(&origin);
        let box_ = sandbox("adopt-untrusted");

        sources::add_source(SourceInstallRequest {
            origin: origin.to_string_lossy().into_owned(),
            ..Default::default()
        })
        .unwrap();
        let report = sync_spec(SyncOptions {
            adopt: true,
            ..SyncOptions::default()
        })
        .unwrap();
        assert_eq!(report.adopted, ["wildy"], "{:?}", report);
        let declared = spec_sources();
        assert_eq!(declared[0].id.as_deref(), Some("wildy"));
        assert!(declared[0].enable.is_empty(), "nothing live to snapshot");
        assert!(declared[0].theme.is_none());
        assert!(rc_text().is_empty(), "untrusted sources activate nothing");

        // Plain sync: the awaiting-trust row stays honest (iron law 2), but
        // the rc and the registry do not move.
        let registry_text =
            fs::read_to_string(sources::sources_root().join("registry.toml")).unwrap();
        let report = sync_spec(SyncOptions::default()).unwrap();
        assert!(
            report
                .rows
                .iter()
                .any(|row| row.id == "wildy" && row.action == "awaiting-trust"),
            "{:?}",
            report.rows
        );
        assert!(rc_text().is_empty());
        assert_eq!(
            fs::read_to_string(sources::sources_root().join("registry.toml")).unwrap(),
            registry_text,
            "registry byte-stable across plain syncs"
        );

        let _ = fs::remove_dir_all(&holder);
        let _ = fs::remove_dir_all(&box_.temp);
    }

    /// The startup memo (F5): a declared-but-missing source whose startup
    /// install fails is attempted ONCE; later startups defer the retry with
    /// one line and no fetch churn; the explicit verb retries and clears.
    #[test]
    fn startup_bootstrap_defers_failed_installs_until_explicit_sync() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let holder = unique_temp_dir("defer-holder");
        let origin = holder.join("not-bash-it");
        fs::create_dir_all(&origin).unwrap();
        // An OMB-shaped tree under a bash-it kind pin: the fingerprint check
        // refuses it (the F6 failure shape, offline).
        fs::write(origin.join("oh-my-bash.sh"), "#!/usr/bin/env bash\n").unwrap();
        let box_ = sandbox("defer");
        let ledger = box_.temp.join("sources/bootstrap-failures.toml");

        spec::save_spec(&PluginSpec {
            schema: None,
            sources: vec![SpecSource {
                target: origin.to_string_lossy().into_owned(),
                id: Some("bash-it".to_string()),
                kind: Some("bash-it".to_string()),
                ref_name: None,
                theme: None,
                enable: vec![],
            }],
        })
        .unwrap();

        // Startup 1: attempted once, fails, memoized.
        let report = sync_spec(SyncOptions {
            startup: true,
            ..SyncOptions::default()
        })
        .unwrap();
        assert!(
            report.rows.iter().any(|row| row.action == "failed"),
            "{:?}",
            report.rows
        );
        let text = fs::read_to_string(&ledger).unwrap();
        assert!(text.contains("[[failure]]"), "{text}");
        assert!(text.contains("bash-it"), "{text}");

        // Startup 2: deferred — no second attempt (no failed row, no
        // staging leftovers under the sources root).
        let report = sync_spec(SyncOptions {
            startup: true,
            ..SyncOptions::default()
        })
        .unwrap();
        assert!(
            !report.rows.iter().any(|row| row.action == "failed"),
            "no retry at startup: {:?}",
            report.rows
        );
        assert_eq!(report.rows[0].action, "deferred", "{:?}", report.rows);
        let leftovers: Vec<String> = fs::read_dir(box_.temp.join("sources"))
            .unwrap()
            .filter_map(Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        assert!(
            leftovers
                .iter()
                .all(|name| name == "bootstrap-failures.toml" || name == "registry.toml"),
            "no staging churn: {leftovers:?}"
        );

        // The explicit verb retries (the failure repeats) and has cleared
        // the memo on the way in — failures are only recorded at startup.
        let report = sync_spec(SyncOptions::default()).unwrap();
        assert!(
            report.rows.iter().any(|row| row.action == "failed"),
            "explicit sync must retry: {:?}",
            report.rows
        );
        let text = fs::read_to_string(&ledger).unwrap();
        assert!(!text.contains("[[failure]]"), "memo cleared: {text}");

        let _ = fs::remove_dir_all(&holder);
        let _ = fs::remove_dir_all(&box_.temp);
    }

    /// One source declared under two spellings (the catalog id and a path,
    /// the exact `niu plugin add` double the 1.3.0 advice invited): sync
    /// keeps the first declaration and merges the rest — diverging enable
    /// lists would otherwise flip the rc on every sync.
    #[test]
    fn duplicate_declarations_of_one_source_merge() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let holder = unique_temp_dir("merge-holder");
        let origin = holder.join("oh-my-fixture");
        fs::create_dir_all(&origin).unwrap();
        write_omb_fixture(&origin);
        let box_ = sandbox("merge");

        sources::add_source(SourceInstallRequest {
            origin: origin.to_string_lossy().into_owned(),
            ..Default::default()
        })
        .unwrap();
        sources::trust_source("oh-my-bash").unwrap();
        let alt = origin.to_string_lossy().replace('\\', "/");
        spec::save_spec(&PluginSpec {
            schema: None,
            sources: vec![
                SpecSource {
                    target: origin.to_string_lossy().into_owned(),
                    id: Some("oh-my-bash".to_string()),
                    kind: None,
                    ref_name: None,
                    theme: None,
                    enable: vec!["git".to_string()],
                },
                SpecSource {
                    target: alt,
                    id: None,
                    kind: None,
                    ref_name: None,
                    theme: None,
                    enable: vec!["npm".to_string()],
                },
            ],
        })
        .unwrap();

        let report = sync_spec(SyncOptions::default()).unwrap();
        assert!(
            report
                .rows
                .iter()
                .any(|row| row.action == "merged" && row.id == "oh-my-bash"),
            "{:?}",
            report.rows
        );
        let declared = spec_sources();
        assert_eq!(declared.len(), 1, "duplicate dropped: {declared:?}");
        assert_eq!(declared[0].enable, ["git"], "the first declaration wins");

        // The healed spec is stable: the next sync is a clean no-op.
        let report = sync_spec(SyncOptions::default()).unwrap();
        assert!(report.clean, "{:?}", report);
        assert_eq!(spec_sources().len(), 1);

        let _ = fs::remove_dir_all(&holder);
        let _ = fs::remove_dir_all(&box_.temp);
    }

    /// niubash#178: a registry that fails to parse (one bad line among good
    /// pinned records) must never be silently rewritten by sync — the read
    /// gives an empty view, and a rewrite from that view would drop every
    /// persisted pin/trust record. Sync's registry write is refused (a
    /// `failed` row, no data loss), the corrupt file survives byte-for-byte,
    /// and a human repair re-enables the normal sync.
    #[test]
    fn sync_never_rewrites_a_corrupt_registry() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let holder = unique_temp_dir("corrupt-holder");
        let origin = holder.join("wildy");
        fs::create_dir_all(&origin).unwrap();
        write_wild_fixture(&origin);
        let box_ = sandbox("corrupt-registry");
        let root = box_.temp.join("sources");

        // A registry with one healthy pinned record plus one unparsable
        // entry (the issue's shape).
        let mut corrupt_text = format!(
            concat!(
                "schema = \"niubash:plugin-source-registry@0.3.0\"\n",
                "\n[[sources]]\n",
                "id = \"wildy\"\n",
                "adapter = \"file\"\n",
                "url = \"{}\"\n",
                "ref = \"local\"\n",
                "version = \"git-abc123\"\n",
                "path = \"{}\"\n",
                "trusted = true\n",
                "license = \"MIT\"\n",
                "checksum_sha256 = \"cafe1234\"\n",
                "installed_at = \"1700000000\"\n",
                "commit_sha = \"abc123def456\"\n",
            ),
            origin.to_string_lossy().replace('\\', "/"),
            root.join("wildy").to_string_lossy().replace('\\', "/"),
        );
        corrupt_text.push_str("\n[[sources]]\nid = \"broken-entry\"\nthis line is not toml =\n");
        fs::create_dir_all(&root).unwrap();
        let registry_path = root.join("registry.toml");
        fs::write(&registry_path, &corrupt_text).unwrap();

        // The spec declares the pinned source; sync cannot see it (the
        // parse fails) and tries to install — the registry write on that
        // path must be refused, not silently rewritten.
        spec::save_spec(&PluginSpec {
            schema: None,
            sources: vec![SpecSource {
                target: origin.to_string_lossy().into_owned(),
                id: None,
                kind: None,
                ref_name: None,
                theme: None,
                enable: vec!["pre.sh".to_string()],
            }],
        })
        .unwrap();
        let report = sync_spec(SyncOptions::default()).unwrap();
        assert!(
            report
                .rows
                .iter()
                .any(|row| row.action == "failed" && row.detail.contains("fails to parse")),
            "sync must refuse loudly, never rewrite: {:?}",
            report.rows
        );
        assert_eq!(
            fs::read_to_string(&registry_path).unwrap(),
            corrupt_text,
            "the corrupt registry (bad line + commit pin) must survive sync"
        );
        assert!(
            root.join("registry.toml.corrupt").is_file(),
            "sidecar snapshot"
        );

        // Consent = a human repairs the file. The next sync proceeds
        // normally (installs, lands untrusted) and the registry parses.
        let repaired = corrupt_text
            .split_once("\n[[sources]]\nid = \"broken-entry\"")
            .expect("the corrupt entry is present")
            .0
            .to_string();
        assert!(repaired.contains("commit_sha"), "{repaired}");
        fs::write(&registry_path, &repaired).unwrap();
        // The pinned record points at a tree that was never fetched (the
        // first sync refused before promotion), so restore it where the pin
        // says it lives — then the repaired sync can adopt and activate it.
        fs::create_dir_all(root.join("wildy")).unwrap();
        write_wild_fixture(&root.join("wildy"));
        let report = sync_spec(SyncOptions::default()).unwrap();
        assert!(
            report
                .rows
                .iter()
                .any(|row| row.id == "wildy" && row.action == "activated"),
            "post-repair sync adopts the pinned record and activates normally: {:?}",
            report.rows
        );
        assert!(sources::registry_corruption().is_none());

        let _ = fs::remove_dir_all(&holder);
        let _ = fs::remove_dir_all(&box_.temp);
    }

    /// niubash#168 (P0): the theme pick moved to oh-my-bash — the wizard's
    /// rc write (G2-routed) — but the spec still carries bash-it's era-1
    /// claim on the SAME shared name, so every sync re-materialized the
    /// bash-it block and reverted the user's theme. The invariant: a pick
    /// written to the rc + spec is the user's latest expressed choice; sync
    /// reconciles a stale claim toward the rc's live state (keeps the
    /// working OSH block, clears the stale claim, drops the theme-only
    /// loser's activation) and then stays there byte-stably.
    #[test]
    fn stale_theme_claim_reconciles_toward_the_rc_and_stays_there() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let holder = unique_temp_dir("claim-holder");
        let bit_origin = holder.join("bit");
        let omb_origin = holder.join("omb");
        fs::create_dir_all(&bit_origin).unwrap();
        fs::create_dir_all(&omb_origin).unwrap();
        write_bash_it_shared_theme_fixture(&bit_origin);
        write_omb_shared_theme_fixture(&omb_origin);
        let box_ = sandbox("claim-reconcile");

        // Era 1 (full-collection era): bash-it registered first, the theme
        // recorded under it. Sync materializes its block and locks it.
        sources::add_source(SourceInstallRequest {
            origin: bit_origin.to_string_lossy().into_owned(),
            ..Default::default()
        })
        .unwrap();
        sources::add_source(SourceInstallRequest {
            origin: omb_origin.to_string_lossy().into_owned(),
            ..Default::default()
        })
        .unwrap();
        sources::trust_source("bash-it").unwrap();
        sources::trust_source("oh-my-bash").unwrap();
        spec::save_spec(&PluginSpec {
            schema: None,
            sources: vec![SpecSource {
                target: bit_origin.to_string_lossy().into_owned(),
                id: Some("bash-it".to_string()),
                kind: None,
                ref_name: None,
                theme: Some("demox".to_string()),
                enable: vec![],
            }],
        })
        .unwrap();
        sync_spec(SyncOptions::default()).unwrap();
        assert!(rc_text().contains("BASH_IT_THEME='demox'"), "{}", rc_text());

        // Era 2 (the wizard pick): the rc is wholesale-rewritten to the
        // oh-my-bash block only, and the spec gains the omb entry (the
        // adopt snapshot) while bash-it's stale claim survives — the exact
        // state a 1.3.x wizard run leaves behind.
        let wizard_block = assets::build_theme_block("oh-my-bash", "demox").expect("theme block");
        let wizard_rc = format!("# wizard rc\n\n{wizard_block}\n");
        fs::write(assets::rc_file(), &wizard_rc).unwrap();
        spec::save_spec(&PluginSpec {
            schema: None,
            sources: vec![
                SpecSource {
                    target: bit_origin.to_string_lossy().into_owned(),
                    id: Some("bash-it".to_string()),
                    kind: None,
                    ref_name: None,
                    theme: Some("demox".to_string()),
                    enable: vec![],
                },
                SpecSource {
                    target: omb_origin.to_string_lossy().into_owned(),
                    id: Some("oh-my-bash".to_string()),
                    kind: None,
                    ref_name: None,
                    theme: Some("demox".to_string()),
                    enable: vec![],
                },
            ],
        })
        .unwrap();

        // Source #1: sync reconciles toward the rc — the OSH block stays
        // byte-identical, the stale bash-it claim is cleared, and the
        // theme-only bash-it activation is NOT re-created.
        let report = sync_spec(SyncOptions::default()).unwrap();
        assert!(
            report
                .rows
                .iter()
                .any(|row| row.id == "bash-it" && row.action == "reconciled"),
            "{:?}",
            report.rows
        );
        assert_eq!(rc_text(), wizard_rc, "the working theme block never flips");
        assert!(rc_text().contains("OSH_THEME='demox'"), "{}", rc_text());
        assert!(
            !rc_text().contains("BASH_IT_THEME"),
            "the stale claim must not re-materialize: {}",
            rc_text()
        );
        // The spec is updated (the stale claim is gone, the pick is pinned).
        let declared = spec_sources();
        assert!(
            declared
                .iter()
                .all(|entry| entry.id.as_deref() != Some("bash-it")),
            "theme-only loser declaration dropped: {declared:?}"
        );
        assert_eq!(declared.len(), 1, "{declared:?}");
        assert_eq!(declared[0].id.as_deref(), Some("oh-my-bash"));
        assert_eq!(declared[0].theme.as_deref(), Some("demox"));

        // Source #2: byte-stable, nothing left to do.
        let report = sync_spec(SyncOptions::default()).unwrap();
        assert!(
            report.rows.iter().all(|row| row.action == "unchanged"),
            "{:?}",
            report.rows
        );
        assert_eq!(rc_text(), wizard_rc, "rc byte-stable across sources");

        let _ = fs::remove_dir_all(&holder);
        let _ = fs::remove_dir_all(&box_.temp);
    }

    /// Same-name theme claims pinned in the spec with nothing live yet: the
    /// exclusive owner is deterministic — oh-my-bash, niu's primary external
    /// framework (the spec-layer twin of the wt61 G2 gallery rank) — and the
    /// other framework's theme-only declaration does not materialize a
    /// competing block.
    #[test]
    fn same_name_claims_pin_to_the_primary_framework_before_anything_is_live() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let holder = unique_temp_dir("pin-holder");
        let bit_origin = holder.join("bit");
        let omb_origin = holder.join("omb");
        fs::create_dir_all(&bit_origin).unwrap();
        fs::create_dir_all(&omb_origin).unwrap();
        write_bash_it_shared_theme_fixture(&bit_origin);
        write_omb_shared_theme_fixture(&omb_origin);
        let box_ = sandbox("claim-pin");

        sources::add_source(SourceInstallRequest {
            origin: bit_origin.to_string_lossy().into_owned(),
            ..Default::default()
        })
        .unwrap();
        sources::add_source(SourceInstallRequest {
            origin: omb_origin.to_string_lossy().into_owned(),
            ..Default::default()
        })
        .unwrap();
        sources::trust_source("bash-it").unwrap();
        sources::trust_source("oh-my-bash").unwrap();
        spec::save_spec(&PluginSpec {
            schema: None,
            sources: vec![
                SpecSource {
                    target: bit_origin.to_string_lossy().into_owned(),
                    id: Some("bash-it".to_string()),
                    kind: None,
                    ref_name: None,
                    theme: Some("demox".to_string()),
                    enable: vec![],
                },
                SpecSource {
                    target: omb_origin.to_string_lossy().into_owned(),
                    id: Some("oh-my-bash".to_string()),
                    kind: None,
                    ref_name: None,
                    theme: Some("demox".to_string()),
                    enable: vec![],
                },
            ],
        })
        .unwrap();

        let report = sync_spec(SyncOptions::default()).unwrap();
        assert!(
            report
                .rows
                .iter()
                .any(|row| row.id == "bash-it" && row.action == "reconciled"),
            "{:?}",
            report.rows
        );
        let rc = rc_text();
        assert!(rc.contains("OSH_THEME='demox'"), "{rc}");
        assert!(!rc.contains("BASH_IT_THEME"), "{rc}");
        let declared = spec_sources();
        assert_eq!(
            declared
                .iter()
                .find(|entry| entry.id.as_deref() == Some("oh-my-bash"))
                .and_then(|entry| entry.theme.as_deref()),
            Some("demox"),
            "the primary framework keeps the claim: {declared:?}"
        );

        // Stable across a second sync.
        let before = rc_text();
        sync_spec(SyncOptions::default()).unwrap();
        assert_eq!(rc_text(), before);

        let _ = fs::remove_dir_all(&holder);
        let _ = fs::remove_dir_all(&box_.temp);
    }

    /// The defaults-as-floor claim model yields to a claimed theme
    /// (niubash#168): a declared-but-selection-less source does not
    /// (re)create its loader-only block when another declared source owns
    /// the theme — the framework would load on top of the pick and revert
    /// it. A block that already exists is left byte-stable.
    #[test]
    fn floor_does_not_resurrect_a_framework_over_a_claimed_theme() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let holder = unique_temp_dir("floor-holder");
        let bit_origin = holder.join("bit");
        let omb_origin = holder.join("omb");
        fs::create_dir_all(&bit_origin).unwrap();
        fs::create_dir_all(&omb_origin).unwrap();
        write_bash_it_shared_theme_fixture(&bit_origin);
        write_omb_shared_theme_fixture(&omb_origin);
        let box_ = sandbox("floor-yield");

        sources::add_source(SourceInstallRequest {
            origin: bit_origin.to_string_lossy().into_owned(),
            ..Default::default()
        })
        .unwrap();
        sources::add_source(SourceInstallRequest {
            origin: omb_origin.to_string_lossy().into_owned(),
            ..Default::default()
        })
        .unwrap();
        sources::trust_source("bash-it").unwrap();
        sources::trust_source("oh-my-bash").unwrap();

        // The post-pick state: oh-my-bash owns the theme (rc block + spec
        // claim); bash-it stays declared but selection-less (no claim, no
        // block — the wizard's rc write removed it).
        let wizard_block = assets::build_theme_block("oh-my-bash", "demox").expect("theme block");
        let wizard_rc = format!("# wizard rc\n\n{wizard_block}\n");
        fs::write(assets::rc_file(), &wizard_rc).unwrap();
        spec::save_spec(&PluginSpec {
            schema: None,
            sources: vec![
                SpecSource {
                    target: bit_origin.to_string_lossy().into_owned(),
                    id: Some("bash-it".to_string()),
                    kind: None,
                    ref_name: None,
                    theme: None,
                    enable: vec![],
                },
                SpecSource {
                    target: omb_origin.to_string_lossy().into_owned(),
                    id: Some("oh-my-bash".to_string()),
                    kind: None,
                    ref_name: None,
                    theme: Some("demox".to_string()),
                    enable: vec![],
                },
            ],
        })
        .unwrap();

        let report = sync_spec(SyncOptions::default()).unwrap();
        assert!(
            report.rows.iter().all(|row| row.action != "activated"),
            "the floor must not resurrect bash-it over the claimed theme: {:?}",
            report.rows
        );
        let rc = rc_text();
        assert!(rc.contains("OSH_THEME='demox'"), "{rc}");
        assert!(
            !rc.contains("niu source bash-it"),
            "no bash-it loader block over the claimed theme: {rc}"
        );

        // Byte-stable across sources.
        sync_spec(SyncOptions::default()).unwrap();
        assert_eq!(rc_text(), wizard_rc);

        let _ = fs::remove_dir_all(&holder);
        let _ = fs::remove_dir_all(&box_.temp);
    }

    /// The `niu plugin add --checksum` hand-off (wt83 #173): the pin rides
    /// the options into the reconciler's install request. A mismatched tree
    /// is refused and registers nothing, a matching one installs untrusted
    /// (the fetch gate + trust flow unchanged), and the pin never leaks
    /// onto other pending declarations.
    #[test]
    fn checksum_pin_refuses_mismatch_honors_match_and_stays_scoped() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let holder = unique_temp_dir("pin-holder");
        let origin_a = holder.join("pinned");
        let origin_b = holder.join("unpinned");
        fs::create_dir_all(&origin_a).unwrap();
        fs::create_dir_all(&origin_b).unwrap();
        write_wild_fixture(&origin_a);
        write_wild_fixture(&origin_b);
        let target_a = origin_a.to_string_lossy().into_owned();
        let target_b = origin_b.to_string_lossy().into_owned();
        let box_ = sandbox("checksum-pin");

        spec::save_spec(&PluginSpec {
            schema: None,
            sources: vec![
                SpecSource {
                    target: target_a.clone(),
                    id: Some("pinned".to_string()),
                    kind: None,
                    ref_name: None,
                    theme: None,
                    enable: vec![],
                },
                SpecSource {
                    target: target_b.clone(),
                    id: Some("unpinned".to_string()),
                    kind: None,
                    ref_name: None,
                    theme: None,
                    enable: vec![],
                },
            ],
        })
        .unwrap();

        // A wrong pin on entry A refuses A's fetch — and only A's: entry B
        // (outside the pin's scope) still installs through the fetch gate.
        let report = sync_spec(SyncOptions {
            checksum_pin: Some((target_a.clone(), "deadbeef".to_string())),
            ..SyncOptions::default()
        })
        .unwrap();
        let pinned_row = report
            .rows
            .iter()
            .find(|row| row.id == "pinned")
            .expect("pinned row");
        assert_eq!(pinned_row.action, "failed", "{:?}", report.rows);
        assert!(
            pinned_row.detail.contains("checksum mismatch"),
            "{:?}",
            report.rows
        );
        assert!(
            report
                .rows
                .iter()
                .any(|row| row.id == "unpinned" && row.action == "awaiting-trust"),
            "{:?}",
            report.rows
        );
        let registered: Vec<String> = read_source_registry()
            .into_iter()
            .map(|record| record.id)
            .collect();
        assert_eq!(registered, ["unpinned"], "refused fetch registers nothing");

        // The correct pin installs — untrusted, fetch gate untouched.
        let good = sources::tree_sha256(&origin_a).unwrap();
        let report = sync_spec(SyncOptions {
            checksum_pin: Some((target_a, good)),
            ..SyncOptions::default()
        })
        .unwrap();
        assert!(
            report
                .rows
                .iter()
                .any(|row| row.id == "pinned" && row.action == "awaiting-trust"),
            "{:?}",
            report.rows
        );
        let pinned = read_source_registry()
            .into_iter()
            .find(|record| record.id == "pinned")
            .expect("pinned installed");
        assert!(!pinned.trusted, "fetch gate unchanged: lands untrusted");
        assert!(rc_text().is_empty(), "untrusted sources activate nothing");

        let _ = fs::remove_dir_all(&holder);
        let _ = fs::remove_dir_all(&box_.temp);
    }

    /// Legacy stranded declarations (wt83 #174): a declared entry whose
    /// startup install failed is memoized and deferred on every later
    /// startup; `sync --prune` is its one-time removal, with a printed
    /// row. A declared-but-never-synced entry (no failure memo) is never
    /// touched by --prune, and an entry this pass installs is kept.
    #[test]
    fn prune_removes_stranded_declarations_and_keeps_pending_ones() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let holder = unique_temp_dir("strand-holder");
        let origin = holder.join("real-tree");
        fs::create_dir_all(&origin).unwrap();
        write_wild_fixture(&origin);
        let missing = holder.join("missing-tree");
        let box_ = sandbox("strand");

        spec::save_spec(&PluginSpec {
            schema: None,
            sources: vec![
                SpecSource {
                    target: missing.to_string_lossy().into_owned(),
                    id: Some("stranded".to_string()),
                    kind: None,
                    ref_name: None,
                    theme: None,
                    enable: vec![],
                },
                SpecSource {
                    target: origin.to_string_lossy().into_owned(),
                    id: Some("pending".to_string()),
                    kind: None,
                    ref_name: None,
                    theme: None,
                    enable: vec![],
                },
            ],
        })
        .unwrap();

        // Startup 1: the broken declaration fails and is memoized; the
        // healthy one installs (untrusted).
        let report = sync_spec(SyncOptions {
            startup: true,
            ..SyncOptions::default()
        })
        .unwrap();
        assert!(
            report
                .rows
                .iter()
                .any(|row| row.id == "stranded" && row.action == "failed"),
            "{:?}",
            report.rows
        );
        assert!(
            fs::read_to_string(bootstrap_failure_path())
                .unwrap()
                .contains("missing-tree"),
            "startup failure memoized"
        );

        // Startup 2: deferred — the every-startup line the issue records.
        let report = sync_spec(SyncOptions {
            startup: true,
            ..SyncOptions::default()
        })
        .unwrap();
        assert!(
            report
                .rows
                .iter()
                .any(|row| row.id == "stranded" && row.action == "deferred"),
            "{:?}",
            report.rows
        );

        // --prune: the stranded declaration comes out of the spec with a
        // printed row; the healthy declaration (installed this run) stays.
        let report = sync_spec(SyncOptions {
            prune: true,
            ..SyncOptions::default()
        })
        .unwrap();
        assert!(
            report.rows.iter().any(|row| row.id == "stranded"
                && row.action == "removed"
                && row.detail.contains("stranded declaration pruned")),
            "{:?}",
            report.rows
        );
        assert!(
            !report
                .rows
                .iter()
                .any(|row| row.id == "pending" && row.action == "removed"),
            "a healthy declaration is never pruned: {:?}",
            report.rows
        );
        let targets: Vec<String> = spec_sources()
            .into_iter()
            .map(|entry| entry.target)
            .collect();
        assert_eq!(targets, [origin.to_string_lossy().into_owned()]);
        assert!(
            !fs::read_to_string(bootstrap_failure_path())
                .unwrap()
                .contains("[[failure]]"),
            "the memo goes with the stranded entry"
        );

        // The healed spec is stable: next startup prints nothing about it.
        let report = sync_spec(SyncOptions {
            startup: true,
            ..SyncOptions::default()
        })
        .unwrap();
        assert!(
            !report.rows.iter().any(|row| row.id == "stranded"),
            "{:?}",
            report.rows
        );

        let _ = fs::remove_dir_all(&holder);
        let _ = fs::remove_dir_all(&box_.temp);
    }

    /// The add-failure rollback helper removes exactly the (target, id)
    /// declaration it is handed and nothing else (wt83 #174).
    #[test]
    fn remove_declared_entry_removes_only_the_matching_pair() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let box_ = sandbox("rollback");
        spec::save_spec(&PluginSpec {
            schema: None,
            sources: vec![
                SpecSource {
                    target: "D:/one".to_string(),
                    id: Some("one".to_string()),
                    kind: None,
                    ref_name: None,
                    theme: None,
                    enable: vec![],
                },
                SpecSource {
                    target: "D:/two".to_string(),
                    id: None,
                    kind: None,
                    ref_name: None,
                    theme: None,
                    enable: vec![],
                },
            ],
        })
        .unwrap();

        assert!(remove_declared_entry("D:/one", Some("one")).unwrap());
        let targets: Vec<String> = spec_sources().into_iter().map(|e| e.target).collect();
        assert_eq!(targets, ["D:/two"]);
        assert!(!remove_declared_entry("D:/one", Some("one")).unwrap());
        assert!(!remove_declared_entry("D:/two", Some("one")).unwrap());

        let _ = fs::remove_dir_all(&box_.temp);
    }

    /// The slowsource P0 pin (owner report 2026-10-04): an IN-SYNC startup
    /// must not notice airplane mode. Every declared source is installed,
    /// trusted, and materialized; the network is dead (all proxies point at
    /// a refused port, so any fetch attempt would fail loudly into the
    /// memo). The startup sync changes nothing, spawns no fetch, and stays
    /// fast — the wall is bounded well under a second in-process.
    #[test]
    fn startup_in_sync_survives_airplane_mode_untouched() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let holder = unique_temp_dir("airplane-holder");
        let origin = holder.join("oh-my-fixture");
        fs::create_dir_all(&origin).unwrap();
        write_omb_fixture(&origin);
        let box_ = sandbox("airplane");

        spec::save_spec(&PluginSpec {
            schema: None,
            sources: vec![SpecSource {
                target: origin.to_string_lossy().into_owned(),
                id: Some("oh-my-bash".to_string()),
                kind: None,
                ref_name: None,
                theme: Some("agnoster".to_string()),
                enable: vec!["git".to_string()],
            }],
        })
        .unwrap();
        sync_spec(SyncOptions::default()).unwrap();
        sources::trust_source("oh-my-bash").unwrap();
        // Materialize once, then confirm the steady state is reached (the
        // second sync is the in-sync no-op the startup form must reproduce).
        let report = sync_spec(SyncOptions::default()).unwrap();
        assert_eq!(report.rows[0].action, "activated", "{:?}", report.rows);
        let report = sync_spec(SyncOptions::default()).unwrap();
        assert_eq!(report.rows[0].action, "unchanged", "{:?}", report.rows);

        // Airplane mode: every proxy points at a refused port, so a single
        // network touch would fail and land in the bootstrap-failure memo.
        let _dead_http = EnvGuard::set("http_proxy", "http://127.0.0.1:9");
        let _dead_https = EnvGuard::set("https_proxy", "http://127.0.0.1:9");
        let _dead_upper_https = EnvGuard::set("HTTPS_PROXY", "http://127.0.0.1:9");
        let _dead_upper_http = EnvGuard::set("HTTP_PROXY", "http://127.0.0.1:9");
        let _dead_all = EnvGuard::set("ALL_PROXY", "http://127.0.0.1:9");

        let registry_before =
            fs::read_to_string(sources::sources_root().join("registry.toml")).unwrap();
        let rc_before = rc_text();
        let spec_before = fs::read_to_string(spec::spec_path()).unwrap();

        let start = std::time::Instant::now();
        let report = sync_spec(SyncOptions {
            startup: true,
            ..SyncOptions::default()
        })
        .unwrap();
        let elapsed = start.elapsed();
        println!("airplane in-sync startup wall: {elapsed:?}");
        assert!(
            elapsed < std::time::Duration::from_secs(5),
            "in-sync startup must stay network-free and fast: {elapsed:?}"
        );
        assert!(
            report.rows.iter().all(|row| row.action == "unchanged"),
            "{:?}",
            report.rows
        );
        assert!(report.clean, "{:?}", report);
        assert!(
            !box_.temp.join("sources/bootstrap-failures.toml").exists(),
            "no fetch attempt may run, so no failure may be memoized"
        );
        assert_eq!(
            fs::read_to_string(sources::sources_root().join("registry.toml")).unwrap(),
            registry_before,
            "registry byte-identical in airplane mode"
        );
        assert_eq!(rc_text(), rc_before, "rc byte-identical in airplane mode");
        assert_eq!(
            fs::read_to_string(spec::spec_path()).unwrap(),
            spec_before,
            "spec byte-identical in airplane mode"
        );

        let _ = fs::remove_dir_all(&holder);
        let _ = fs::remove_dir_all(&box_.temp);
    }

    /// The other half of the slowsource P0: when a declared source IS
    /// missing and the network is blackholed, the startup fetch attempt is
    /// BOUNDED (`sources::STARTUP_FETCH_BUDGET`, tuned down here via the
    /// env override), the failure is memoized, and the next startup defers
    /// instead of paying again. Unbounded, one TLS-burning attempt cost the
    /// owner 5.26s and a dropped SYN would hang git for minutes.
    #[test]
    fn startup_fetch_budget_bounds_the_first_attempt_and_memoizes() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let box_ = sandbox("budget");
        let ledger = box_.temp.join("sources/bootstrap-failures.toml");

        // RFC 5737 TEST-NET-3: never routable, so a plain git clone hangs
        // in the connect phase instead of failing fast with a reset.
        spec::save_spec(&PluginSpec {
            schema: None,
            sources: vec![SpecSource {
                target: "https://203.0.113.1/repo.git".to_string(),
                id: Some("blackhole".to_string()),
                kind: None,
                ref_name: None,
                theme: None,
                enable: vec![],
            }],
        })
        .unwrap();
        let _budget = EnvGuard::set("NIU_STARTUP_FETCH_BUDGET_MS", "300");

        let start = std::time::Instant::now();
        let report = sync_spec(SyncOptions {
            startup: true,
            ..SyncOptions::default()
        })
        .unwrap();
        let elapsed = start.elapsed();
        println!("budgeted startup attempt wall: {elapsed:?}");
        // Bounded: the budget (300ms) plus process slack, never the
        // minutes a blackholed TCP connect would otherwise cost.
        assert!(
            elapsed < std::time::Duration::from_secs(10),
            "the startup fetch must respect the budget: {elapsed:?}"
        );
        assert!(
            report.rows.iter().any(|row| row.action == "failed"),
            "{:?}",
            report.rows
        );
        let memo = fs::read_to_string(&ledger).unwrap();
        // The memo keys by the resolved origin (what the deferral lookup
        // matches), not the spec id.
        assert!(memo.contains("203.0.113.1"), "{memo}");
        assert!(memo.contains("[[failure]]"), "{memo}");

        // The memo quiets the NEXT startup (no second attempt).
        let report = sync_spec(SyncOptions {
            startup: true,
            ..SyncOptions::default()
        })
        .unwrap();
        assert_eq!(report.rows[0].action, "deferred", "{:?}", report.rows);

        // The explicit verb retries and clears the memo on the way in.
        let report = sync_spec(SyncOptions::default()).unwrap();
        assert!(
            report.rows.iter().any(|row| row.action == "failed"),
            "explicit sync must retry: {:?}",
            report.rows
        );
        let memo = fs::read_to_string(&ledger).unwrap();
        assert!(!memo.contains("[[failure]]"), "memo cleared: {memo}");

        let _ = fs::remove_dir_all(&box_.temp);
    }
}
