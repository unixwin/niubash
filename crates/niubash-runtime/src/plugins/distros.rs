//! Plugin collections — the LazyVim extras pattern for niu (study §8,
//! §10.2): a collection ("distro") is a *data manifest* listing recipe ids;
//! built-in collections ship in the product, imported collections come
//! from a repo or directory with the same manifest shape (LazyVim's
//! user-extras rule: anything the distro can do, a user repo can do).
//! All manifests are JSON since the 1.5.0 product-TOML deletion; the
//! pre-1.5.0 TOML data on disk is converted exactly once by
//! [`migrate_legacy_distros`] (`niu plugin migrate`) — no runtime path
//! reads TOML.
//! `apply` walks the entries through [`super::recipes::install`]
//! — sources land installed; activation stays a separate explicit verb.
//!
//! Imported collections live under `~/.niubash/distros/<name>/` with a
//! `niu-collection.json` manifest at the root; the registry
//! (`~/.niubash/distros/registry.json`) records provenance.

use std::fs;
use std::path::{Path, PathBuf};
// The import clone goes through `sources::git_clone_bounded`; `Command`
// remains a test-only tool here (seeding offline mirrors).
#[cfg(test)]
use std::process::Command;

use anyhow::{anyhow, bail, Context};
use serde::{Deserialize, Serialize};

use super::recipes::{self, RecipeInstall};

/// Schema marker for collection manifests and the distro registry.
pub const COLLECTION_SCHEMA: &str = "niubash:plugin-collection@1";
/// Manifest file name inside a collection repo/directory.
pub const COLLECTION_MANIFEST: &str = "niu-collection.json";

/// Pre-1.5.0 TOML manifest name. Read exactly once, by
/// [`migrate_legacy_distros`]; no runtime path looks for it.
const LEGACY_COLLECTION_MANIFEST: &str = "niu-collection.toml";

/// Registry file name inside the distros root.
const DISTRO_REGISTRY: &str = "registry.json";

/// Pre-1.5.0 TOML registry name (migrate-only, like the manifest).
const LEGACY_DISTRO_REGISTRY: &str = "registry.toml";

/// Built-in official collections (compiled in). Built-in and imported
/// collections share the one JSON manifest shape
/// (`COLLECTION_MANIFEST` below).
const BUILTIN_COLLECTIONS: &[(&str, &str)] = &[
    (
        "minimal",
        include_str!("../../assets/plugins/collections/minimal.json"),
    ),
    (
        "recommended",
        include_str!("../../assets/plugins/collections/recommended.json"),
    ),
    (
        "full",
        include_str!("../../assets/plugins/collections/full.json"),
    ),
];

/// One collection manifest row.
#[derive(Debug, Clone, Deserialize)]
pub struct Collection {
    #[serde(default)]
    pub schema: Option<String>,
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub entry: Vec<CollectionEntry>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CollectionEntry {
    pub recipe: String,
}

/// Where a collection came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CollectionOrigin {
    Builtin,
    Imported { path: PathBuf, origin: String },
}

#[derive(Debug, Clone)]
pub struct CollectionListing {
    pub collection: Collection,
    pub origin: CollectionOrigin,
}

/// The one manifest parser: built-in and imported collections alike are
/// JSON (the 1.5.0 product-TOML deletion). The pre-1.5.0 TOML form is
/// parsed only inside [`migrate_legacy_distros`].
fn parse_manifest(text: &str) -> anyhow::Result<Collection> {
    let collection: Collection =
        serde_json::from_str(text).context("parsing the collection manifest")?;
    validate_manifest(collection)
}

fn validate_manifest(collection: Collection) -> anyhow::Result<Collection> {
    if let Some(schema) = &collection.schema {
        if schema != COLLECTION_SCHEMA {
            bail!("unsupported collection schema '{schema}' (expected '{COLLECTION_SCHEMA}')");
        }
    }
    if collection.name.trim().is_empty() {
        bail!("collection manifest has no name");
    }
    Ok(collection)
}

/// Validate that every entry resolves to a known recipe (health-style:
/// name every unknown id, study §10.1).
pub fn validate_collection(collection: &Collection) -> anyhow::Result<()> {
    let unknown: Vec<&str> = collection
        .entry
        .iter()
        .map(|entry| entry.recipe.as_str())
        .filter(|recipe| recipes::recipe(recipe).is_none())
        .collect();
    if !unknown.is_empty() {
        bail!(
            "collection '{}' references unknown recipes: {}",
            collection.name,
            unknown.join(", ")
        );
    }
    Ok(())
}

/// All collections: built-ins first (stable order), then imported.
pub fn collections() -> Vec<CollectionListing> {
    let mut out = Vec::new();
    for (_, text) in BUILTIN_COLLECTIONS {
        if let Ok(collection) = parse_manifest(text) {
            out.push(CollectionListing {
                collection,
                origin: CollectionOrigin::Builtin,
            });
        }
    }
    for record in read_distro_registry() {
        let manifest = record.path.join(COLLECTION_MANIFEST);
        if let Ok(text) = fs::read_to_string(&manifest) {
            if let Ok(collection) = parse_manifest(&text) {
                out.push(CollectionListing {
                    collection,
                    origin: CollectionOrigin::Imported {
                        path: record.path.clone(),
                        origin: record.origin.clone(),
                    },
                });
            }
        }
    }
    out
}

pub fn collection(name: &str) -> Option<CollectionListing> {
    collections()
        .into_iter()
        .find(|listing| listing.collection.name == name)
}

/// Record in `~/.niubash/distros/registry.json`.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct DistroRecord {
    pub name: String,
    /// Where the tree was imported from (git url or local path).
    pub origin: String,
    pub path: PathBuf,
}

/// The registry file shape (`registry.json`).
#[derive(Deserialize, Serialize)]
struct DistroRegistry {
    /// Forward compatibility: unread on load (a mismatched registry
    /// degrades to "no imported collections", never a crash).
    #[serde(default)]
    schema: Option<String>,
    #[serde(default)]
    distro: Vec<DistroRecord>,
}

pub fn distros_root() -> PathBuf {
    if let Some(value) = std::env::var_os("NIU_PLUGIN_DISTROS_ROOT") {
        let path = PathBuf::from(value);
        if !path.as_os_str().is_empty() {
            return path;
        }
    }
    crate::path_utils::shell_home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".niubash")
        .join("distros")
}

fn distro_registry_path() -> PathBuf {
    distros_root().join(DISTRO_REGISTRY)
}

fn read_distro_registry() -> Vec<DistroRecord> {
    let Ok(text) = fs::read_to_string(distro_registry_path()) else {
        return Vec::new();
    };
    serde_json::from_str::<DistroRegistry>(&text)
        .map(|file| file.distro)
        .unwrap_or_default()
}

/// Import a collection from a git URL/shorthand or a local directory.
/// The tree lands under `~/.niubash/distros/<name>/`; the manifest must
/// parse and every entry must resolve. Re-importing the same origin is the
/// refresh path (the destination is replaced); a *different* origin taking
/// an imported name is refused, as is any name colliding with a built-in
/// (built-ins are listed first and would shadow the import).
pub fn import(origin: &str) -> anyhow::Result<Collection> {
    let normalized = super::sources::normalize_origin(origin);
    let staging = distros_root().join(format!(
        ".staging-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    fs::create_dir_all(&staging)?;
    let result = (|| -> anyhow::Result<()> {
        let path = Path::new(&normalized);
        if path.is_dir() {
            copy_tree(path, &staging)?;
        } else {
            // niubash#293/#294: the import clone is bounded by the verb
            // budget (the process tree dies at the deadline) and carries
            // the proxy environment, exactly like the source-install
            // transport — a dead origin used to hang `distro import`
            // indefinitely on a bare `git clone ... .status()`.
            super::sources::git_clone_bounded(&normalized, &staging)?;
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_dir_all(&staging);
        result?;
    }

    let text = fs::read_to_string(staging.join(COLLECTION_MANIFEST)).map_err(|err| {
        let _ = fs::remove_dir_all(&staging);
        anyhow!("no {COLLECTION_MANIFEST} at the root of '{normalized}': {err}")
    })?;
    // `inspect_err` is 1.76; the crate MSRV is 1.70 — same cleanup, no
    // new API (the staging dir must not survive a failed import).
    let collection = match parse_manifest(&text) {
        Ok(collection) => collection,
        Err(err) => {
            let _ = fs::remove_dir_all(&staging);
            return Err(err);
        }
    };
    if let Err(err) = validate_collection(&collection) {
        let _ = fs::remove_dir_all(&staging);
        return Err(err);
    }

    // Name-collision policy (health-style: name the object and the repair
    // verb, study §10.1). Built-ins win every listing lookup, so importing
    // under a built-in name would silently shadow; an imported name may
    // only be refreshed from the origin that owns it.
    for listing in collections() {
        if listing.collection.name != collection.name {
            continue;
        }
        match listing.origin {
            CollectionOrigin::Builtin => {
                let _ = fs::remove_dir_all(&staging);
                bail!(
                    "collection name '{}' collides with a built-in collection; \
                     rename it in its {COLLECTION_MANIFEST}",
                    collection.name
                );
            }
            CollectionOrigin::Imported { origin, .. } if origin != normalized => {
                let _ = fs::remove_dir_all(&staging);
                bail!(
                    "collection '{}' is already imported from '{origin}'; \
                     remove it first: niu plugin distro remove {}",
                    collection.name,
                    collection.name
                );
            }
            CollectionOrigin::Imported { .. } => {} // same origin: refresh below
        }
    }

    let dest = distros_root().join(&collection.name);
    if dest.exists() {
        fs::remove_dir_all(&dest)?;
    }
    fs::rename(&staging, &dest)?;

    let mut registry = read_distro_registry();
    registry.retain(|record| record.name != collection.name);
    registry.push(DistroRecord {
        name: collection.name.clone(),
        origin: normalized.clone(),
        path: dest.clone(),
    });
    write_distro_registry(&registry)?;
    Ok(collection)
}

fn write_distro_registry(registry: &[DistroRecord]) -> anyhow::Result<()> {
    fs::create_dir_all(distros_root())?;
    let file = DistroRegistry {
        schema: Some(COLLECTION_SCHEMA.to_string()),
        distro: registry.to_vec(),
    };
    let mut body =
        serde_json::to_string_pretty(&file).context("serializing the distro registry")?;
    body.push('\n');
    fs::write(distro_registry_path(), body)?;
    Ok(())
}

/// Outcome of [`migrate_legacy_distros`]: what the one-shot bridge moved.
#[derive(Debug, Clone)]
pub struct DistroMigrationReport {
    /// The legacy registry file, renamed aside (present iff one existed).
    pub registry_backup: Option<PathBuf>,
    /// Names of imported collections whose manifest was converted.
    pub collections: Vec<String>,
    /// Legacy manifest files renamed aside after conversion.
    pub manifest_backups: Vec<PathBuf>,
    /// Legacy manifests left in place, with the reason (never guessed at).
    pub skipped: Vec<String>,
}

/// One-time migration from the pre-1.5.0 TOML distro data: the registry
/// (`registry.toml` → `registry.json`) and every imported collection's
/// manifest (`niu-collection.toml` → `niu-collection.json`). This is the
/// ONLY code that still reads the TOML forms — the runtime never does
/// (the same one-shot pattern as `niu config migrate` on the config
/// face). Converted legacy files are renamed to
/// `<name>.migrated-<UTC timestamp>`, never deleted. When there is no
/// legacy data this returns `Ok(None)`; a legacy registry that does not
/// parse refuses the whole migration before anything on disk changes;
/// a single unconvertible manifest is skipped and named in the report
/// while the rest still migrate.
pub fn migrate_legacy_distros() -> anyhow::Result<Option<DistroMigrationReport>> {
    let root = distros_root();
    let legacy_registry_path = root.join(LEGACY_DISTRO_REGISTRY);

    // Parse the legacy registry first: a registry niu cannot read
    // refuses the whole migration with everything left untouched.
    let legacy_registry_text = match fs::read_to_string(&legacy_registry_path) {
        Ok(text) => Some(text),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
        Err(err) => {
            return Err(err)
                .with_context(|| format!("could not read {}", legacy_registry_path.display()))
        }
    };
    let mut records: Vec<DistroRecord> = Vec::new();
    if let Some(text) = &legacy_registry_text {
        #[derive(Deserialize)]
        struct LegacyRegistry {
            #[serde(default)]
            distro: Vec<DistroRecord>,
        }
        records = toml::from_str::<LegacyRegistry>(text)
            .with_context(|| {
                format!(
                    "could not parse {} — fix or remove the file by hand; \
                     niu never rewrites a registry it cannot read",
                    legacy_registry_path.display()
                )
            })?
            .distro;
    }

    // Candidate trees: the registry's records, plus any stray imported
    // tree still carrying a legacy manifest (a tree whose record was
    // lost must not strand its manifest either).
    let mut trees: Vec<PathBuf> = records.iter().map(|record| record.path.clone()).collect();
    if let Ok(entries) = fs::read_dir(&root) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir()
                && path.join(LEGACY_COLLECTION_MANIFEST).is_file()
                && !trees.contains(&path)
            {
                trees.push(path);
            }
        }
    }
    if legacy_registry_text.is_none() && trees.is_empty() {
        return Ok(None);
    }

    // Merge into the JSON registry: records already there win by name,
    // legacy records they do not know are carried over.
    if !records.is_empty() {
        let mut merged = read_distro_registry();
        for record in records {
            if !merged.iter().any(|existing| existing.name == record.name) {
                merged.push(record);
            }
        }
        write_distro_registry(&merged)?;
    }

    let mut report = DistroMigrationReport {
        registry_backup: None,
        collections: Vec::new(),
        manifest_backups: Vec::new(),
        skipped: Vec::new(),
    };
    for tree in &trees {
        let legacy_manifest = tree.join(LEGACY_COLLECTION_MANIFEST);
        let Ok(text) = fs::read_to_string(&legacy_manifest) else {
            continue;
        };
        match convert_legacy_manifest(&text) {
            Ok((name, json)) => {
                fs::write(tree.join(COLLECTION_MANIFEST), json)?;
                report
                    .manifest_backups
                    .push(rename_legacy_file(&legacy_manifest)?);
                report.collections.push(name);
            }
            Err(err) => {
                report
                    .skipped
                    .push(format!("{}: {err:#}", legacy_manifest.display()));
            }
        }
    }
    if legacy_registry_text.is_some() {
        report.registry_backup = Some(rename_legacy_file(&legacy_registry_path)?);
    }
    Ok(Some(report))
}

/// Convert one legacy TOML collection manifest to its JSON form,
/// validated through the same rules as a live manifest. The conversion
/// goes through `toml::Value` so fields this build does not model
/// survive the move untouched.
fn convert_legacy_manifest(text: &str) -> anyhow::Result<(String, String)> {
    let collection: Collection =
        toml::from_str(text).context("parsing the legacy collection manifest")?;
    let collection = validate_manifest(collection)?;
    let value: toml::Value =
        toml::from_str(text).context("parsing the legacy collection manifest")?;
    let mut json =
        serde_json::to_string_pretty(&value).context("serializing the collection manifest")?;
    json.push('\n');
    Ok((collection.name, json))
}

/// Rename a converted legacy file aside: `<name>.migrated-<UTC stamp>`
/// (the same backup shape as `config::migrate_legacy_config`).
fn rename_legacy_file(path: &Path) -> anyhow::Result<PathBuf> {
    let stamp = legacy_stamp();
    let mut candidate = PathBuf::from(format!("{}.migrated-{stamp}", path.display()));
    let mut suffix = 1u32;
    while candidate.exists() {
        candidate = PathBuf::from(format!("{}.migrated-{stamp}-{suffix}", path.display()));
        suffix += 1;
    }
    fs::rename(path, &candidate).with_context(|| {
        format!(
            "could not rename {} to {}",
            path.display(),
            candidate.display()
        )
    })?;
    Ok(candidate)
}

/// UTC timestamp shaped for a filename: `YYYYMMDD-HHMMSSZ`
/// (same shape as the config migrate's stamp).
fn legacy_stamp() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}{month:02}{day:02}-{:02}{:02}{:02}Z",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

/// Howard Hinnant's days-from-civil inverse (proleptic Gregorian).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = mp + if mp < 10 { 3 } else { -9 };
    let year = y + i64::from(m <= 2);
    (year, m as u32, d as u32)
}

/// Remove an imported collection (built-ins cannot be removed).
pub fn remove(name: &str) -> anyhow::Result<()> {
    let listing = collection(name).ok_or_else(|| anyhow!("unknown collection '{name}'"))?;
    match listing.origin {
        CollectionOrigin::Builtin => bail!("'{name}' is built in and cannot be removed"),
        CollectionOrigin::Imported { path, .. } => {
            let mut registry = read_distro_registry();
            registry.retain(|record| record.name != name);
            write_distro_registry(&registry)?;
            if path.is_dir() {
                fs::remove_dir_all(&path)?;
            }
            Ok(())
        }
    }
}

/// Outcome of applying a collection: per-entry install reports (failures
/// collected, not thrown — a bad entry never kills the rest, lazy.nvim
/// `Spec:log` pattern, study §10.1) plus the undo targets.
#[derive(Debug, Clone)]
pub struct CollectionApply {
    pub name: String,
    pub reports: Vec<RecipeInstall>,
    /// Entries that failed: (recipe id, error message).
    pub failures: Vec<(String, String)>,
    /// Source ids installed by this apply (undo: `niu plugin source remove`).
    pub installed_sources: Vec<String>,
}

/// Apply a collection: walk the entries through the recipe drivers.
/// Executable-tool entries recommend package managers (download retraction
/// 2026-10-04); they install nothing and fail nothing.
pub fn apply(name: &str) -> anyhow::Result<CollectionApply> {
    let listing = collection(name).ok_or_else(|| anyhow!("unknown collection '{name}'"))?;
    validate_collection(&listing.collection)?;
    let before_sources: Vec<String> = super::sources::read_source_registry()
        .into_iter()
        .map(|record| record.id)
        .collect();

    let mut reports = Vec::new();
    let mut failures = Vec::new();
    for entry in &listing.collection.entry {
        match recipes::install(&entry.recipe) {
            Ok(report) => reports.push(report),
            Err(err) => failures.push((entry.recipe.clone(), format!("{err:#}"))),
        }
    }

    let installed_sources: Vec<String> = super::sources::read_source_registry()
        .into_iter()
        .map(|record| record.id)
        .filter(|id| !before_sources.contains(id))
        .collect();
    Ok(CollectionApply {
        name: listing.collection.name,
        reports,
        failures,
        installed_sources,
    })
}

fn copy_tree(src: &Path, dest: &Path) -> anyhow::Result<()> {
    fs::create_dir_all(dest)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let path = entry.path();
        // Guard: never recurse into the destination itself (importing a
        // directory that contains the distros root would otherwise copy
        // the staging dir into itself without bound).
        if path.starts_with(dest) {
            continue;
        }
        let target = dest.join(entry.file_name());
        if path.is_dir() {
            copy_tree(&path, &target)?;
        } else if path.is_file() {
            fs::copy(&path, &target)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::PROCESS_STATE_LOCK;

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

    #[test]
    fn builtin_collections_parse_and_validate() {
        let listings = collections();
        let names: Vec<&str> = listings
            .iter()
            .map(|l| l.collection.name.as_str())
            .collect();
        assert_eq!(names, ["minimal", "recommended", "full"], "{names:?}");
        for listing in &listings {
            validate_collection(&listing.collection).unwrap_or_else(|err| {
                panic!("built-in '{}' invalid: {err}", listing.collection.name)
            });
        }
    }

    #[test]
    fn builtin_recommended_rides_the_default_omb_theme() {
        let listing = collection("recommended").unwrap();
        let recipes: Vec<&str> = listing
            .collection
            .entry
            .iter()
            .map(|e| e.recipe.as_str())
            .collect();
        assert!(recipes.contains(&"oh-my-bash"), "{recipes:?}");
        assert!(recipes.contains(&"omb-theme-robbyrussell"), "{recipes:?}");
        assert!(recipes.contains(&"bash-completion"), "{recipes:?}");
    }

    #[test]
    fn manifest_schema_mismatch_is_refused() {
        let err = parse_manifest("{\"schema\": \"niubash:plugin-collection@99\", \"name\": \"x\"}")
            .unwrap_err()
            .to_string();
        assert!(err.contains("plugin-collection@1"), "{err}");
    }

    #[test]
    fn unknown_entries_are_named_in_validation() {
        let collection = Collection {
            schema: None,
            name: "test".into(),
            description: String::new(),
            entry: vec![
                CollectionEntry {
                    recipe: "oh-my-bash".into(),
                },
                CollectionEntry {
                    recipe: "no-such-recipe".into(),
                },
            ],
        };
        let err = validate_collection(&collection).unwrap_err().to_string();
        assert!(err.contains("no-such-recipe"), "{err}");
    }

    #[test]
    fn import_from_local_directory_registers_and_applies() {
        let temp = std::env::temp_dir().join(format!(
            "niu-distros-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&temp).unwrap();
        let source = temp.join("collection-src");
        fs::create_dir_all(&source).unwrap();
        fs::write(
            source.join(COLLECTION_MANIFEST),
            "{\"schema\": \"niubash:plugin-collection@1\", \"name\": \"test-distro\", \
             \"description\": \"d\", \"entry\": [{\"recipe\": \"bash-preexec\"}]}",
        )
        .unwrap();
        // A real driver target so apply has something to install: point the
        // recipe's generic source at a local fixture? bash-preexec clones
        // from GitHub — not in tests. Validate import + listing only; the
        // apply path is covered by the recipes unit tests.
        let _guard = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _distros = EnvGuard::set(
            "NIU_PLUGIN_DISTROS_ROOT",
            &temp.join("distros").to_string_lossy(),
        );
        let imported = import(source.to_string_lossy().as_ref()).expect("import");
        assert_eq!(imported.name, "test-distro");
        let listings = collections();
        let names: Vec<&str> = listings
            .iter()
            .map(|l| l.collection.name.as_str())
            .collect();
        assert!(names.contains(&"test-distro"), "{names:?}");

        // Same origin again = refresh (no error, entry set still resolves).
        import(source.to_string_lossy().as_ref()).expect("re-import refreshes");

        // A different origin under the same name is refused, naming the
        // owning origin and the repair verb.
        let other = temp.join("collection-other");
        fs::create_dir_all(&other).unwrap();
        fs::write(
            other.join(COLLECTION_MANIFEST),
            "{\"schema\": \"niubash:plugin-collection@1\", \"name\": \"test-distro\", \
             \"entry\": [{\"recipe\": \"bash-preexec\"}]}",
        )
        .unwrap();
        let err = import(other.to_string_lossy().as_ref())
            .unwrap_err()
            .to_string();
        assert!(err.contains("already imported"), "{err}");
        assert!(err.contains("niu plugin distro remove"), "{err}");

        remove("test-distro").expect("remove");
        assert!(!collections()
            .iter()
            .any(|l| l.collection.name == "test-distro"));
        drop(_distros);
        let _ = fs::remove_dir_all(&temp);
    }

    /// niubash#293: a `distro import` from a dead git origin fails BOUNDED
    /// at the verb budget (`NIU_PLUGIN_FETCH_TIMEOUT_MS` here) and names
    /// the origin — the import used to hang on a bare
    /// `git clone ... .status()` until the user killed niu.
    #[test]
    fn import_from_a_dead_origin_fails_bounded_at_the_verb_budget() {
        if Command::new("git")
            .arg("--version")
            .status()
            .map(|status| !status.success())
            .unwrap_or(true)
        {
            eprintln!("skipping bounded-import: git not usable on PATH");
            return;
        }
        let temp = std::env::temp_dir().join(format!(
            "niu-distros-bounded-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&temp).unwrap();
        let _guard = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _distros = EnvGuard::set(
            "NIU_PLUGIN_DISTROS_ROOT",
            &temp.join("distros").to_string_lossy(),
        );
        let _budget = EnvGuard::set("NIU_PLUGIN_FETCH_TIMEOUT_MS", "300");

        let start = std::time::Instant::now();
        let err = import("https://203.0.113.1/stalled-collection.git")
            .expect_err("a blackholed import must fail at the verb budget");
        assert!(
            start.elapsed() < std::time::Duration::from_secs(10),
            "the import must be bounded, took {:?}",
            start.elapsed()
        );
        let message = err.to_string();
        assert!(
            message.contains("203.0.113.1"),
            "the failure must name the origin: {message}"
        );
        // No staging churn survives the failed import.
        let leftovers: Vec<String> = fs::read_dir(temp.join("distros"))
            .unwrap()
            .filter_map(Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        assert!(
            leftovers.iter().all(|name| name == "registry.json"),
            "no staging leftovers: {leftovers:?}"
        );

        let _ = fs::remove_dir_all(&temp);
    }

    #[test]
    fn import_refuses_builtin_name_collision() {
        let temp = std::env::temp_dir().join(format!(
            "niu-distros-builtin-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&temp).unwrap();
        // The import source must live outside the distros root (the staging
        // dir is created under it — copy_tree guards, but keep the fixture
        // shaped like a real import).
        let source = temp.join("collection-builtin");
        fs::create_dir_all(&source).unwrap();
        fs::write(
            source.join(COLLECTION_MANIFEST),
            "{\"schema\": \"niubash:plugin-collection@1\", \"name\": \"minimal\", \
             \"entry\": [{\"recipe\": \"bash-completion\"}]}",
        )
        .unwrap();
        let _guard = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _distros = EnvGuard::set(
            "NIU_PLUGIN_DISTROS_ROOT",
            &temp.join("distros").to_string_lossy(),
        );
        let err = import(source.to_string_lossy().as_ref())
            .unwrap_err()
            .to_string();
        assert!(err.contains("built-in"), "{err}");
    }

    #[test]
    fn apply_activates_assets_on_an_installed_manager() {
        // Network-free apply path: with the oh-my-bash fixture installed,
        // a collection entry that rides it activates locally (the
        // recipes::install manager-asset branch). The collection itself is
        // imported from a local directory so no git clone runs.
        let _guard = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let temp = std::env::temp_dir().join(format!(
            "niu-distros-apply-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&temp).unwrap();
        let _sources = EnvGuard::set(
            "NIU_PLUGIN_SOURCES_ROOT",
            &temp.join("sources").to_string_lossy(),
        );
        let _distros = EnvGuard::set(
            "NIU_PLUGIN_DISTROS_ROOT",
            &temp.join("distros").to_string_lossy(),
        );
        let fixture = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/sources/oh-my-bash");
        super::super::sources::add_source(super::super::sources::SourceInstallRequest {
            adapter: None,
            origin: fixture.to_string_lossy().into_owned(),
            ..Default::default()
        })
        .expect("fixture source add");

        let source = temp.join("collection-src");
        fs::create_dir_all(&source).unwrap();
        fs::write(
            source.join(COLLECTION_MANIFEST),
            "{\"schema\": \"niubash:plugin-collection@1\", \"name\": \"test-apply\", \
             \"entry\": [{\"recipe\": \"omb-theme-robbyrussell\"}]}",
        )
        .unwrap();
        import(source.to_string_lossy().as_ref()).expect("import test collection");
        let outcome = apply("test-apply").expect("apply");
        assert_eq!(outcome.name, "test-apply");
        assert_eq!(outcome.reports.len(), 1, "{:?}", outcome.reports);
        assert_eq!(outcome.reports[0].recipe_id, "omb-theme-robbyrussell");

        // Cleanup: drop the source and the imported tree (both under temp).
        let _ = super::super::sources::remove_source("oh-my-bash");
    }

    #[test]
    fn builtin_cannot_be_removed() {
        let err = remove("minimal").unwrap_err().to_string();
        assert!(err.contains("built in"), "{err}");
    }

    /// Stage `src` (a tests/fixtures/sources tree) as a real git repository
    /// at `dest` — the offline mirror `git clone` resolves to (the d2
    /// smoke-journey pattern: `NIU_MIRRORS` insteadOf, the documented §14.8
    /// transport layer).
    fn seed_mirror_repo(src: &Path, dest: &Path) {
        fn copy_dir(from: &Path, to: &Path) {
            fs::create_dir_all(to).unwrap();
            for entry in fs::read_dir(from).unwrap() {
                let entry = entry.unwrap();
                let target = to.join(entry.file_name());
                if entry.path().is_dir() {
                    copy_dir(&entry.path(), &target);
                } else {
                    fs::copy(entry.path(), target).unwrap();
                }
            }
        }
        copy_dir(src, dest);
        for args in [
            vec!["init".to_string(), "-q".to_string()],
            vec![
                "-c".into(),
                "user.email=distros-test@niu".into(),
                "-c".into(),
                "user.name=niu-distros-test".into(),
                "add".into(),
                "-A".into(),
            ],
            vec![
                "-c".into(),
                "user.email=distros-test@niu".into(),
                "-c".into(),
                "user.name=niu-distros-test".into(),
                "commit".into(),
                "-qm".into(),
                "seed".into(),
            ],
        ] {
            let status = Command::new("git")
                .arg("-C")
                .arg(dest)
                .args(&args)
                .status()
                .expect("git mirror seed command");
            assert!(status.success(), "git {args:?} failed seeding {dest:?}");
        }
    }

    /// Sandbox root for the offline apply tests: isolated sources/distros/
    /// mirrors plus a local `git` probe (skip loudly-env-free, like the d2
    /// smoke journey's `which git` guard).
    struct OfflineApply {
        root: PathBuf,
    }

    impl OfflineApply {
        fn new(tag: &str) -> Option<Self> {
            if Command::new("git")
                .arg("--version")
                .status()
                .map(|status| !status.success())
                .unwrap_or(true)
            {
                eprintln!("skipping {tag}: git not usable on PATH");
                return None;
            }
            let root = std::env::temp_dir().join(format!(
                "niu-distros-{tag}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            fs::create_dir_all(root.join("home")).unwrap();
            fs::create_dir_all(root.join("sources")).unwrap();
            fs::create_dir_all(root.join("distros")).unwrap();
            Some(Self { root })
        }

        fn env_guards(&self) -> Vec<(&'static str, String)> {
            let mirror_base = format!(
                "file:///{}/",
                self.root
                    .join("mirror")
                    .display()
                    .to_string()
                    .replace('\\', "/")
            );
            fs::write(
                self.root.join("mirrors.toml"),
                format!(
                    "# offline apply test: rewrite GitHub fetches to the seeded local mirror\n\
                     schema = \"niubash:mirrors@0.1.0\"\n\
                     active = \"custom\"\n\n\
                     [github]\n\
                     git_instead_of = \"{mirror_base}\"\n"
                ),
            )
            .unwrap();
            vec![
                (
                    "NIU_PLUGIN_SOURCES_ROOT",
                    self.root.join("sources").to_string_lossy().into_owned(),
                ),
                (
                    "NIU_PLUGIN_DISTROS_ROOT",
                    self.root.join("distros").to_string_lossy().into_owned(),
                ),
                (
                    "NIU_MIRRORS",
                    self.root
                        .join("mirrors.toml")
                        .to_string_lossy()
                        .into_owned(),
                ),
                (
                    "HOME",
                    self.root.join("home").to_string_lossy().into_owned(),
                ),
                (
                    "USERPROFILE",
                    self.root.join("home").to_string_lossy().into_owned(),
                ),
            ]
        }
    }

    impl Drop for OfflineApply {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    /// Import a local collection manifest carrying exactly `recipes` and
    /// apply it. Returns the outcome.
    fn import_and_apply(tag: &str, sandbox: &OfflineApply, recipes: &[&str]) -> CollectionApply {
        let source = sandbox.root.join("collection-src").join(tag);
        fs::create_dir_all(&source).unwrap();
        let entries: Vec<String> = recipes
            .iter()
            .map(|recipe| format!("{{\"recipe\": \"{recipe}\"}}"))
            .collect();
        let manifest = format!(
            "{{\"schema\": \"{COLLECTION_SCHEMA}\", \"name\": \"{tag}\", \
             \"description\": \"offline independent-recipe apply\", \"entry\": [{}]}}",
            entries.join(", ")
        );
        fs::write(source.join(COLLECTION_MANIFEST), manifest).unwrap();
        import(source.to_string_lossy().as_ref()).expect("import offline collection");
        apply(tag).expect("apply collects failures, never throws")
    }

    /// niubash#171: an independent recipe entry (a completion-script repo,
    /// not a framework and not an executable tool) installs through the
    /// collection apply via the plugin driver's own git install — clone,
    /// file-source detection, recipe-entry validation, plain landing.
    /// Fully offline: the recipe's canonical GitHub origin resolves through
    /// the seeded local mirror, and the registry keeps the canonical URL
    /// (the mirror is transport-only).
    #[test]
    fn apply_installs_an_independent_recipe_entry_offline() {
        let _guard = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let Some(sandbox) = OfflineApply::new("independent") else {
            return;
        };
        let _envs: Vec<_> = sandbox
            .env_guards()
            .into_iter()
            .map(|(name, value)| EnvGuard::set(name, &value))
            .collect();
        let fixtures =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/sources");
        seed_mirror_repo(
            &fixtures.join("complete-alias"),
            &sandbox.root.join("mirror/cykerway/complete-alias.git"),
        );
        seed_mirror_repo(
            &fixtures.join("git-flow-completion"),
            &sandbox
                .root
                .join("mirror/bobthecow/git-flow-completion.git"),
        );

        let outcome = import_and_apply(
            "test-independent",
            &sandbox,
            &["complete-alias", "git-flow-completion"],
        );
        // Every entry installed: honest report, no failures.
        assert!(outcome.failures.is_empty(), "{:?}", outcome.failures);
        let ids: Vec<&str> = outcome
            .reports
            .iter()
            .map(|report| report.recipe_id.as_str())
            .collect();
        assert_eq!(ids, ["complete-alias", "git-flow-completion"], "{ids:?}");
        for report in &outcome.reports {
            assert!(
                report.summary.contains("installed source"),
                "honest install report: {}",
                report.summary
            );
        }
        // The recipe's own enable verb, spelled out (niubash#647: there
        // is no trust step in the chain).
        assert_eq!(
            outcome.reports[0].next,
            vec!["niu plugin enable complete-alias/complete_alias"],
            "{:?}",
            outcome.reports[0].next
        );
        // The journal's source list names exactly what this apply landed
        // (it feeds the finish screen's undo receipts).
        assert_eq!(
            outcome.installed_sources,
            ["complete-alias", "git-flow-completion"],
            "{:?}",
            outcome.installed_sources
        );
        // Canonical origins in the registry — the mirror never leaks into
        // provenance (§14.8 transport-only iron invariant).
        for id in ["complete-alias", "git-flow-completion"] {
            let record = crate::plugins::sources::read_source_registry()
                .into_iter()
                .find(|record| record.id == id)
                .unwrap_or_else(|| panic!("{id} must be registered"));
            assert!(record.path.is_dir(), "{id} must land installed");
            assert!(
                record.url.starts_with("https://github.com/"),
                "canonical origin, not the mirror: {}",
                record.url
            );
        }
    }

    /// The golden journey's J2 bar, made deterministic and offline: the
    /// **built-in full collection** applies with every entry installed —
    /// themes, frameworks, and (niubash#171) the independent recipe
    /// entries alike, each through its real clone → detect → entry
    /// validation path via the seeded mirror. Every entry's repo must be
    /// seeded here and its entry file present in the fixture: adding a
    /// collection entry means adding its fixture + seed (the same audit
    /// the recipes.rs upstream-root table enforces).
    #[test]
    fn apply_full_collection_installs_every_entry_offline() {
        let _guard = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let Some(sandbox) = OfflineApply::new("full") else {
            return;
        };
        let _envs: Vec<_> = sandbox
            .env_guards()
            .into_iter()
            .map(|(name, value)| EnvGuard::set(name, &value))
            .collect();
        let fixtures =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/sources");
        let mirror = sandbox.root.join("mirror");
        for (fixture, owner_repo) in [
            ("oh-my-bash", "ohmybash/oh-my-bash.git"),
            ("bash-it", "Bash-it/bash-it.git"),
            ("bash-completion", "scop/bash-completion.git"),
            ("bash-preexec", "rcaloras/bash-preexec.git"),
            ("complete-alias", "cykerway/complete-alias.git"),
            ("fzf-git.sh", "junegunn/fzf-git.sh.git"),
            ("git-flow-completion", "bobthecow/git-flow-completion.git"),
        ] {
            seed_mirror_repo(&fixtures.join(fixture), &mirror.join(owner_repo));
        }
        // bash-sensible's entry file lives at its fixture root and its
        // repo root has no other *.sh/*.bash — same shape as upstream.
        seed_mirror_repo(
            &fixtures.join("bash-sensible"),
            &mirror.join("mrzool/bash-sensible.git"),
        );

        let full = collection("full").expect("built-in full collection");
        let entries: Vec<&str> = full
            .collection
            .entry
            .iter()
            .map(|entry| entry.recipe.as_str())
            .collect();
        assert_eq!(
            entries,
            [
                "oh-my-bash",
                "omb-theme-robbyrussell",
                "bash-it",
                "bash-completion",
                "bash-preexec",
                "complete-alias",
                "fzf-git.sh",
                "bash-sensible",
                "git-flow-completion",
            ],
            "the full manifest drifted: update this test's seeds"
        );

        let outcome = apply("full").expect("apply collects failures, never throws");
        assert!(
            outcome.failures.is_empty(),
            "every collection entry must install: {:?}",
            outcome.failures
        );
        assert_eq!(
            outcome.reports.len(),
            entries.len(),
            "{:?}",
            outcome.reports
        );
        // Nine reports, eight landed sources (the theme rides the
        // oh-my-bash manager source), all installed.
        let mut landed = outcome.installed_sources.clone();
        landed.sort();
        assert_eq!(
            landed,
            [
                "bash-completion",
                "bash-it",
                "bash-preexec",
                "bash-sensible",
                "complete-alias",
                "fzf-git.sh",
                "git-flow-completion",
                "oh-my-bash",
            ],
            "{:?}",
            outcome.installed_sources
        );
    }

    /// wt61 G3 discipline, niubash#171 edition: an entry whose upstream
    /// drifted away from the audited entry file fails *honestly* — named
    /// in the apply failures with the real reason — while the healthy
    /// entries still install.
    #[test]
    fn apply_names_a_drifted_entry_and_installs_the_rest() {
        let _guard = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let Some(sandbox) = OfflineApply::new("drifted") else {
            return;
        };
        let _envs: Vec<_> = sandbox
            .env_guards()
            .into_iter()
            .map(|(name, value)| EnvGuard::set(name, &value))
            .collect();
        // The drifted origin: the fixture tree WITHOUT the audited entry
        // file — what upstream renaming `complete_alias` would look like.
        let fixtures =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/sources");
        let drifted = sandbox.root.join("drifted-src/complete-alias");
        seed_mirror_repo(&fixtures.join("complete-alias"), &drifted);
        fs::remove_file(drifted.join("complete_alias")).unwrap();
        let _ = Command::new("git")
            .arg("-C")
            .arg(&drifted)
            .args([
                "-c",
                "user.email=distros-test@niu",
                "-c",
                "user.name=niu-distros-test",
            ])
            .args(["commit", "-qam", "drift: entry file moved away"])
            .status()
            .expect("drift commit");
        seed_mirror_repo(
            &fixtures.join("git-flow-completion"),
            &sandbox
                .root
                .join("mirror/bobthecow/git-flow-completion.git"),
        );
        // complete-alias's recipe origin rewrites through the same mirror
        // root — re-home the drifted tree onto the canonical path layout.
        let canonical = sandbox.root.join("mirror/cykerway/complete-alias.git");
        fs::create_dir_all(canonical.parent().unwrap()).unwrap();
        fs::rename(&drifted, &canonical).unwrap();

        let outcome = import_and_apply(
            "test-drifted",
            &sandbox,
            &["complete-alias", "git-flow-completion"],
        );
        // The drifted entry is named, with the honest reason.
        assert_eq!(outcome.failures.len(), 1, "{:?}", outcome.failures);
        assert_eq!(outcome.failures[0].0, "complete-alias");
        assert!(
            outcome.failures[0]
                .1
                .contains("entry file 'complete_alias' not found"),
            "{:?}",
            outcome.failures[0]
        );
        // The healthy entry still installed, and only it counts as landed.
        assert_eq!(outcome.reports.len(), 1, "{:?}", outcome.reports);
        assert_eq!(outcome.reports[0].recipe_id, "git-flow-completion");
        assert_eq!(
            outcome.installed_sources,
            ["git-flow-completion"],
            "{:?}",
            outcome.installed_sources
        );
    }

    /// Journey run-13 observation (G3): a collection apply with a failed
    /// entry must report it, and `installed_sources` must name only what
    /// actually landed. Offline and deterministic: the manager comes from
    /// the local fixture, and `omb-theme-90210` is a compiled-in recipe
    /// whose asset the fixture tree does not carry, so its enable fails.
    #[test]
    fn apply_with_a_failing_entry_reports_it_and_installs_the_rest() {
        let _guard = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let temp = std::env::temp_dir().join(format!(
            "niu-distros-fail-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&temp).unwrap();
        let home = temp.join("home");
        std::fs::create_dir_all(&home).unwrap();
        // Full sandbox: the healthy entry activates through assets::enable,
        // which writes the spec and the rc — never the real profile.
        let _home = EnvGuard::set("HOME", &home.to_string_lossy());
        let _userprofile = EnvGuard::set("USERPROFILE", &home.to_string_lossy());
        let _spec = EnvGuard::set(
            "NIU_PLUGIN_SPEC",
            &temp.join("plugins.toml").to_string_lossy(),
        );
        let _sources = EnvGuard::set(
            "NIU_PLUGIN_SOURCES_ROOT",
            &temp.join("sources").to_string_lossy(),
        );
        let _distros = EnvGuard::set(
            "NIU_PLUGIN_DISTROS_ROOT",
            &temp.join("distros").to_string_lossy(),
        );
        let fixture = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/sources/oh-my-bash");
        super::super::sources::add_source(super::super::sources::SourceInstallRequest {
            adapter: None,
            origin: fixture.to_string_lossy().into_owned(),
            ..Default::default()
        })
        .expect("fixture source add");

        let source = temp.join("collection-src");
        fs::create_dir_all(&source).unwrap();
        fs::write(
            source.join(COLLECTION_MANIFEST),
            "{\"schema\": \"niubash:plugin-collection@1\", \"name\": \"test-partial\", \
             \"entry\": [{\"recipe\": \"omb-theme-agnoster\"}, {\"recipe\": \"omb-theme-90210\"}]}",
        )
        .unwrap();
        import(source.to_string_lossy().as_ref()).expect("import test-partial");
        let outcome = apply("test-partial").expect("apply collects failures, never throws");
        // The failing entry is named, with the honest reason.
        assert_eq!(outcome.failures.len(), 1, "{:?}", outcome.failures);
        assert_eq!(outcome.failures[0].0, "omb-theme-90210");
        // The healthy entry still installed (activated through the
        // fixture manager) — a bad entry never kills the rest.
        assert_eq!(outcome.reports.len(), 1, "{:?}", outcome.reports);
        assert_eq!(outcome.reports[0].recipe_id, "omb-theme-agnoster");
        // No NEW source landed (the manager pre-existed) — the honest
        // report of what this apply itself installed.
        assert!(
            outcome.installed_sources.is_empty(),
            "{:?}",
            outcome.installed_sources
        );

        // Cleanup: drop the source and the imported tree (both under temp).
        let _ = super::super::sources::remove_source("oh-my-bash");
    }

    fn legacy_temp(tag: &str) -> PathBuf {
        let temp = std::env::temp_dir().join(format!(
            "niu-distros-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&temp).unwrap();
        temp
    }

    fn legacy_manifest_toml(name: &str) -> String {
        format!(
            "schema = \"{COLLECTION_SCHEMA}\"\nname = \"{name}\"\ndescription = \"d\"\n\n\
             [[entry]]\nrecipe = \"bash-preexec\"\n"
        )
    }

    /// The runtime has no TOML read path: a registered tree carrying only
    /// the legacy manifest stays invisible until `niu plugin migrate`
    /// converts it.
    #[test]
    fn toml_manifests_are_not_read_by_the_runtime() {
        let temp = legacy_temp("nolegacy");
        let _guard = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _distros = EnvGuard::set(
            "NIU_PLUGIN_DISTROS_ROOT",
            &temp.join("distros").to_string_lossy(),
        );
        let tree = temp.join("distros").join("ghost");
        fs::create_dir_all(&tree).unwrap();
        fs::write(
            tree.join(LEGACY_COLLECTION_MANIFEST),
            legacy_manifest_toml("ghost"),
        )
        .unwrap();
        // The JSON registry knows the tree; only its manifest is legacy.
        write_distro_registry(&[DistroRecord {
            name: "ghost".into(),
            origin: "local".into(),
            path: tree,
        }])
        .unwrap();
        assert!(
            !collections().iter().any(|l| l.collection.name == "ghost"),
            "a TOML-only manifest must stay invisible to the runtime"
        );
        let _ = fs::remove_dir_all(&temp);
    }

    #[test]
    fn migrate_converts_the_legacy_registry_and_manifests() {
        let temp = legacy_temp("migrate");
        let _guard = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _distros = EnvGuard::set(
            "NIU_PLUGIN_DISTROS_ROOT",
            &temp.join("distros").to_string_lossy(),
        );
        let root = temp.join("distros");
        let tree = root.join("test-distro");
        fs::create_dir_all(&tree).unwrap();
        fs::write(
            tree.join(LEGACY_COLLECTION_MANIFEST),
            legacy_manifest_toml("test-distro"),
        )
        .unwrap();
        // A stray tree whose registry record was lost: its manifest is
        // converted too (but no provenance is invented for it).
        let stray = root.join("stray-distro");
        fs::create_dir_all(&stray).unwrap();
        fs::write(
            stray.join(LEGACY_COLLECTION_MANIFEST),
            legacy_manifest_toml("stray-distro"),
        )
        .unwrap();
        // The registry in the exact shape the pre-1.5.0 writer produced.
        let tree_toml = tree.display().to_string().replace('\\', "\\\\");
        fs::write(
            root.join(LEGACY_DISTRO_REGISTRY),
            format!(
                "# Imported plugin collections (niu plugin distro import).\n\
                 schema = \"{COLLECTION_SCHEMA}\"\n\n\
                 [[distro]]\nname = \"test-distro\"\norigin = \"local-src\"\npath = \"{tree_toml}\"\n"
            ),
        )
        .unwrap();

        let report = migrate_legacy_distros()
            .expect("migrate")
            .expect("legacy data present");
        let mut converted = report.collections.clone();
        converted.sort();
        assert_eq!(converted, ["stray-distro", "test-distro"], "{report:?}");
        assert!(report.registry_backup.is_some(), "{report:?}");
        assert!(report.skipped.is_empty(), "{report:?}");

        // The JSON registry carries the record and the runtime lists the
        // collection again; the stray tree stays unregistered.
        let records = read_distro_registry();
        assert_eq!(records.len(), 1, "{records:?}");
        assert_eq!(records[0].name, "test-distro");
        assert!(collection("test-distro").is_some());
        assert!(collection("stray-distro").is_none());

        // JSON manifests on disk; legacy files renamed aside, not deleted.
        assert!(tree.join(COLLECTION_MANIFEST).is_file());
        assert!(stray.join(COLLECTION_MANIFEST).is_file());
        assert!(!tree.join(LEGACY_COLLECTION_MANIFEST).exists());
        assert!(!root.join(LEGACY_DISTRO_REGISTRY).exists());
        let root_names: Vec<String> = fs::read_dir(&root)
            .unwrap()
            .filter_map(Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        assert!(
            root_names
                .iter()
                .any(|name| name.starts_with("registry.toml.migrated-")),
            "{root_names:?}"
        );
        let tree_names: Vec<String> = fs::read_dir(&tree)
            .unwrap()
            .filter_map(Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        assert!(
            tree_names
                .iter()
                .any(|name| name.starts_with("niu-collection.toml.migrated-")),
            "{tree_names:?}"
        );

        // Second run: nothing left to migrate.
        assert!(migrate_legacy_distros().unwrap().is_none());
        let _ = fs::remove_dir_all(&temp);
    }

    #[test]
    fn migrate_without_legacy_data_is_a_no_op() {
        let temp = legacy_temp("migrate-none");
        let _guard = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _distros = EnvGuard::set(
            "NIU_PLUGIN_DISTROS_ROOT",
            &temp.join("distros").to_string_lossy(),
        );
        // Empty root: nothing to do.
        assert!(migrate_legacy_distros().unwrap().is_none());
        // A JSON-only root is left byte-identical.
        write_distro_registry(&[DistroRecord {
            name: "kept".into(),
            origin: "local".into(),
            path: temp.join("distros").join("kept"),
        }])
        .unwrap();
        let before = fs::read(distro_registry_path()).unwrap();
        assert!(migrate_legacy_distros().unwrap().is_none());
        assert_eq!(fs::read(distro_registry_path()).unwrap(), before);
        let _ = fs::remove_dir_all(&temp);
    }

    #[test]
    fn migrate_refuses_an_unreadable_registry_and_leaves_it() {
        let temp = legacy_temp("migrate-bad");
        let _guard = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _distros = EnvGuard::set(
            "NIU_PLUGIN_DISTROS_ROOT",
            &temp.join("distros").to_string_lossy(),
        );
        let root = temp.join("distros");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join(LEGACY_DISTRO_REGISTRY), "this is not [ toml").unwrap();
        let err = migrate_legacy_distros().unwrap_err().to_string();
        assert!(err.contains("could not parse"), "{err}");
        // Untouched: the legacy file stays, no JSON registry appears.
        assert!(root.join(LEGACY_DISTRO_REGISTRY).is_file());
        assert!(!distro_registry_path().exists());
        let _ = fs::remove_dir_all(&temp);
    }

    #[test]
    fn migrate_skips_an_unconvertible_manifest_and_migrates_the_rest() {
        let temp = legacy_temp("migrate-skip");
        let _guard = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _distros = EnvGuard::set(
            "NIU_PLUGIN_DISTROS_ROOT",
            &temp.join("distros").to_string_lossy(),
        );
        let root = temp.join("distros");
        let good = root.join("good-distro");
        let bad = root.join("bad-distro");
        fs::create_dir_all(&good).unwrap();
        fs::create_dir_all(&bad).unwrap();
        fs::write(
            good.join(LEGACY_COLLECTION_MANIFEST),
            legacy_manifest_toml("good-distro"),
        )
        .unwrap();
        fs::write(
            bad.join(LEGACY_COLLECTION_MANIFEST),
            "schema = \"niubash:plugin-collection@99\"\nname = \"bad-distro\"\n",
        )
        .unwrap();
        let mut registry_text = format!(
            "# Imported plugin collections (niu plugin distro import).\n\
             schema = \"{COLLECTION_SCHEMA}\"\n"
        );
        for (name, path) in [("good-distro", &good), ("bad-distro", &bad)] {
            let path_toml = path.display().to_string().replace('\\', "\\\\");
            registry_text.push_str(&format!(
                "\n[[distro]]\nname = \"{name}\"\norigin = \"local\"\npath = \"{path_toml}\"\n"
            ));
        }
        fs::write(root.join(LEGACY_DISTRO_REGISTRY), registry_text).unwrap();

        let report = migrate_legacy_distros()
            .expect("migrate")
            .expect("legacy data present");
        assert_eq!(report.collections, ["good-distro"], "{report:?}");
        assert_eq!(report.skipped.len(), 1, "{report:?}");
        assert!(report.skipped[0].contains("bad-distro"), "{report:?}");
        // The good tree converted; the bad tree is left exactly as it was.
        assert!(good.join(COLLECTION_MANIFEST).is_file());
        assert!(bad.join(LEGACY_COLLECTION_MANIFEST).is_file());
        assert!(!bad.join(COLLECTION_MANIFEST).exists());
        // The registry still migrated (both records carried over).
        assert_eq!(read_distro_registry().len(), 2);
        let _ = fs::remove_dir_all(&temp);
    }
}
