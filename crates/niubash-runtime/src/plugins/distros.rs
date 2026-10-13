//! Plugin collections — the LazyVim extras pattern for niu (study §8,
//! §10.2): a collection ("distro") is a *data manifest* listing recipe ids;
//! built-in collections ship in the product, imported collections are the
//! same format from a repo or directory, byte-identical in shape
//! (LazyVim's user-extras rule: anything the distro can do, a user repo
//! can do). `apply` walks the entries through [`super::recipes::install`]
//! and **never auto-trusts** — sources land untrusted with their review
//! verb printed, per owner ruling 2026-10-02.
//!
//! Imported collections live under `~/.niubash/distros/<name>/` with a
//! `niu-collection.toml` manifest at the root; the registry
//! (`~/.niubash/distros/registry.toml`) records provenance.

use std::fs;
use std::path::{Path, PathBuf};
// The import clone goes through `sources::git_clone_bounded`; `Command`
// remains a test-only tool here (seeding offline mirrors).
#[cfg(test)]
use std::process::Command;

use anyhow::{anyhow, bail, Context};
use serde::Deserialize;

use super::recipes::{self, RecipeInstall};

/// Schema marker for collection manifests and the distro registry.
pub const COLLECTION_SCHEMA: &str = "niubash:plugin-collection@1";
/// Manifest file name inside a collection repo/directory.
pub const COLLECTION_MANIFEST: &str = "niu-collection.toml";

/// Built-in official collections (compiled in, same format as imports).
const BUILTIN_COLLECTIONS: &[(&str, &str)] = &[
    (
        "minimal",
        include_str!("../../assets/plugins/collections/minimal.toml"),
    ),
    (
        "recommended",
        include_str!("../../assets/plugins/collections/recommended.toml"),
    ),
    (
        "full",
        include_str!("../../assets/plugins/collections/full.toml"),
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

fn parse_manifest(text: &str) -> anyhow::Result<Collection> {
    let collection: Collection = toml::from_str(text).context("parsing the collection manifest")?;
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

/// Record in `~/.niubash/distros/registry.toml`.
#[derive(Debug, Clone, Deserialize)]
pub struct DistroRecord {
    pub name: String,
    /// Where the tree was imported from (git url or local path).
    pub origin: String,
    pub path: PathBuf,
    pub imported_at: String,
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
    distros_root().join("registry.toml")
}

fn read_distro_registry() -> Vec<DistroRecord> {
    let Ok(text) = fs::read_to_string(distro_registry_path()) else {
        return Vec::new();
    };
    #[derive(Deserialize)]
    struct Registry {
        /// Forward compatibility: unread (a mismatched registry degrades to
        /// "no imported collections", never a crash).
        #[serde(default, skip_serializing)]
        #[allow(dead_code)]
        schema: Option<String>,
        #[serde(default)]
        distro: Vec<DistroRecord>,
    }
    toml::from_str::<Registry>(&text)
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
        imported_at: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs().to_string())
            .unwrap_or_default(),
    });
    write_distro_registry(&registry)?;
    Ok(collection)
}

fn write_distro_registry(registry: &[DistroRecord]) -> anyhow::Result<()> {
    fs::create_dir_all(distros_root())?;
    let mut body = format!(
        "# Imported plugin collections (niu plugin distro import).\nschema = \"{COLLECTION_SCHEMA}\"\n"
    );
    for record in registry {
        body.push_str(&format!(
            "\n[[distro]]\nname = {}\norigin = {}\npath = {}\nimported_at = {}\n",
            toml_quote(&record.name),
            toml_quote(&record.origin),
            toml_quote(&record.path.display().to_string()),
            toml_quote(&record.imported_at)
        ));
    }
    fs::write(distro_registry_path(), body)?;
    Ok(())
}

fn toml_quote(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
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

/// Apply a collection: walk the entries through the recipe drivers. Trust
/// is never granted here — each report carries the review verb.
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
        let err = parse_manifest("schema = \"niubash:plugin-collection@99\"\nname = \"x\"\n")
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
            "# test collection\nschema = \"niubash:plugin-collection@1\"\nname = \"test-distro\"\ndescription = \"d\"\n\n[[entry]]\nrecipe = \"bash-preexec\"\n",
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
            "schema = \"niubash:plugin-collection@1\"\nname = \"test-distro\"\n\n[[entry]]\nrecipe = \"bash-preexec\"\n",
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
            leftovers.iter().all(|name| name == "registry.toml"),
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
            "schema = \"niubash:plugin-collection@1\"\nname = \"minimal\"\n\n[[entry]]\nrecipe = \"bash-completion\"\n",
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
    fn apply_activates_assets_on_a_trusted_manager() {
        // Network-free apply path: with the oh-my-bash fixture installed and
        // trusted, a collection entry that rides it activates locally (the
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
        super::super::sources::trust_source("oh-my-bash").expect("fixture trust");

        let source = temp.join("collection-src");
        fs::create_dir_all(&source).unwrap();
        fs::write(
            source.join(COLLECTION_MANIFEST),
            "schema = \"niubash:plugin-collection@1\"\nname = \"test-apply\"\n\n[[entry]]\nrecipe = \"omb-theme-robbyrussell\"\n",
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
        let mut manifest = format!(
            "schema = \"{COLLECTION_SCHEMA}\"\nname = \"{tag}\"\ndescription = \"offline independent-recipe apply\"\n\n"
        );
        for recipe in recipes {
            manifest.push_str(&format!("[[entry]]\nrecipe = \"{recipe}\"\n\n"));
        }
        fs::write(source.join(COLLECTION_MANIFEST), manifest).unwrap();
        import(source.to_string_lossy().as_ref()).expect("import offline collection");
        apply(tag).expect("apply collects failures, never throws")
    }

    /// niubash#171: an independent recipe entry (a completion-script repo,
    /// not a framework and not an executable tool) installs through the
    /// collection apply via the plugin driver's own git install — clone,
    /// file-source detection, recipe-entry validation, untrusted landing.
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
                report.summary.contains("untrusted"),
                "the trust gate must hold: {}",
                report.summary
            );
        }
        // The recipe's own review + enable verbs, spelled out.
        assert_eq!(
            outcome.reports[0].next,
            vec![
                "niu plugin trust complete-alias",
                "niu plugin enable complete-alias/complete_alias",
            ],
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
            assert!(!record.trusted, "{id} must land untrusted");
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
        // oh-my-bash manager source), all untrusted.
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
        super::super::sources::trust_source("oh-my-bash").expect("fixture trust");

        let source = temp.join("collection-src");
        fs::create_dir_all(&source).unwrap();
        fs::write(
            source.join(COLLECTION_MANIFEST),
            "schema = \"niubash:plugin-collection@1\"\nname = \"test-partial\"\n\n\
             [[entry]]\nrecipe = \"omb-theme-agnoster\"\n\n\
             [[entry]]\nrecipe = \"omb-theme-90210\"\n",
        )
        .unwrap();
        import(source.to_string_lossy().as_ref()).expect("import test-partial");
        let outcome = apply("test-partial").expect("apply collects failures, never throws");
        // The failing entry is named, with the honest reason.
        assert_eq!(outcome.failures.len(), 1, "{:?}", outcome.failures);
        assert_eq!(outcome.failures[0].0, "omb-theme-90210");
        // The healthy entry still installed (activated through the trusted
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
}
