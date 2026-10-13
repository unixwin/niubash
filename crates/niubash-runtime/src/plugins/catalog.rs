//! Curated catalog of well-known external plugin sources (owner ruling
//! 2026-10-02: no vendoring — sources enter the product as a preset list of
//! official origins, fetched on explicit user command; license obligations
//! stay between the user and upstream).
//!
//! The catalog is a *shorthand table*, the same role `owner/repo` plays for
//! vim-plug/lazy.nvim: `niu plugin add oh-my-bash` resolves to the official
//! origin and clones it. It confers no trust — every catalog install still
//! lands untrusted and waits for `niu plugin trust`/`sign` (§12.2).

use super::sources::{builtin_source_adapters, PluginSourceAdapter};

/// One curated entry: a known plugin manager and its official origin.
#[derive(Debug, Clone)]
pub struct CatalogEntry {
    /// Source/adapter id (`niu plugin add <id>`).
    pub id: &'static str,
    /// Official git origin the id expands to.
    pub origin: &'static str,
    /// License of the fetched tree, as stated by the project (annotation
    /// only — GPL projects are fetched on demand and never redistributed
    /// by niubash).
    pub license: &'static str,
    /// One-line summary shown by discover/add.
    pub summary: &'static str,
}

/// The compiled-in catalog. Derived from the adapters (single source of
/// truth for ids/origins/licenses) so the two can never drift.
pub fn catalog_entries() -> Vec<CatalogEntry> {
    builtin_source_adapters()
        .iter()
        .filter_map(|adapter: &&'static dyn PluginSourceAdapter| {
            let origin = adapter.default_origin()?;
            Some(CatalogEntry {
                id: adapter.id(),
                origin,
                license: adapter.license(),
                summary: adapter.summary(),
            })
        })
        .collect()
}

/// Look up a catalog entry by id.
pub fn catalog_entry(id: &str) -> Option<CatalogEntry> {
    catalog_entries().into_iter().find(|entry| entry.id == id)
}

/// True when the origin is one of the curated catalog origins (niubash#298):
/// its first fetch rides a compiled-in, reviewed origin, so the delivery has
/// an integrity reference a wild URL lacks.
pub fn is_catalog_origin(origin: &str) -> bool {
    let origin = origin.trim();
    catalog_entries().iter().any(|entry| entry.origin == origin)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_covers_the_three_managers_with_origins() {
        let entries = catalog_entries();
        let ids: Vec<&str> = entries.iter().map(|entry| entry.id).collect();
        // Registration order: the flagship manager (oh-my-bash) first.
        assert_eq!(ids, ["oh-my-bash", "bash-it", "bash-completion"], "{ids:?}");
        for entry in &entries {
            assert!(entry.origin.starts_with("https://github.com/"), "{entry:?}");
            assert!(!entry.license.is_empty(), "{entry:?}");
            assert!(!entry.summary.is_empty(), "{entry:?}");
        }
        // The GPL project is annotated as such (fetch-on-demand only).
        let bash_completion = catalog_entry("bash-completion").unwrap();
        assert_eq!(bash_completion.license, "GPL-2.0-or-later");
    }

    #[test]
    fn catalog_entry_lookup_misses_cleanly() {
        assert!(catalog_entry("nope").is_none());
    }
}
