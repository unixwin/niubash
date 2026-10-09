//! Release pre-install manifest contract (niubash#189).
//!
//! `scripts/release/preinstall.json` is release-CI policy, not tribal
//! memory: the workflow installs `packages` via `winuxcmd wpm install`,
//! materializes `shims` as winuxcmd.exe hardlinks, and must never ship a
//! name recorded in `excluded`. These tests pin the schema and the
//! owner-ordered exclusions so a manifest edit cannot silently re-admit a
//! forbidden package (compression tools, goawk, `link`) or drop the fail-open
//! disclosure. The workflow reads the same file; a schema break fails
//! `cargo test` here before it can fail a release.

use serde_json::Value;
use std::path::PathBuf;

fn manifest_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("scripts/release/preinstall.json")
}

fn load_manifest() -> Value {
    let raw = std::fs::read_to_string(manifest_path())
        .expect("preinstall manifest must exist at scripts/release/preinstall.json");
    serde_json::from_str(&raw).expect("preinstall manifest must be valid JSON")
}

fn string_list(value: &Value) -> Vec<String> {
    value
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// Every package/shim/exclusion entry is a lowercase wpm package name
/// (`^[a-z0-9][a-z0-9+-]*$` — the same shape the wpm index uses).
fn assert_name_shape(name: &str, context: &str) {
    assert!(
        !name.is_empty()
            && name.chars().next().is_some_and(|c| c.is_ascii_lowercase())
            && name
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '+' || c == '-'),
        "{context}: name {name:?} is not a valid wpm package name"
    );
}

#[test]
fn manifest_schema_is_complete() {
    let manifest = load_manifest();

    assert_eq!(
        manifest.get("schema").and_then(Value::as_u64),
        Some(1),
        "schema version must be 1; bump requires updating this test file too"
    );
    for key in ["description", "packages", "shims", "excluded", "notes"] {
        assert!(
            manifest.get(key).is_some(),
            "manifest is missing required key {key:?}"
        );
    }

    // packages: non-empty, each with a name, purpose, and clean shape.
    let packages = manifest
        .get("packages")
        .and_then(Value::as_array)
        .expect("packages must be an array");
    assert!(
        !packages.is_empty(),
        "a release pre-install manifest with zero packages is pointless; delete the step instead"
    );
    let mut names = Vec::new();
    for package in packages {
        let name = package
            .get("name")
            .and_then(Value::as_str)
            .expect("every package entry needs a string name");
        assert_name_shape(name, "packages");
        assert!(
            package
                .get("purpose")
                .and_then(Value::as_str)
                .is_some_and(|purpose| !purpose.trim().is_empty()),
            "package {name:?} needs a non-empty purpose (why it ships)"
        );
        names.push(name.to_string());
    }
    let unique = names
        .iter()
        .cloned()
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        unique.len(),
        names.len(),
        "duplicate package entries: {names:?}"
    );
}

/// niubash#209 acceptance, pinned in CI policy: the must-install bundle is
/// gawk (niubash#189) plus niugit / ripgrep / fd. Dropping any of them from
/// the manifest silently re-breaks `niu setup` full distro on a gitless
/// machine, so their presence is a contract, not a preference.
#[test]
fn must_install_bundle_is_present() {
    let manifest = load_manifest();
    let package_names = string_list_of(&manifest, "packages");
    for required in ["gawk", "niugit", "ripgrep", "fd"] {
        assert!(
            package_names.iter().any(|name| name == required),
            "must-install package {required:?} is missing from the manifest (niubash#189/#209)"
        );
    }
}

#[test]
fn shims_target_installed_packages_and_do_not_collide() {
    let manifest = load_manifest();
    let package_names: Vec<String> = string_list_of(&manifest, "packages");
    let shims = manifest
        .get("shims")
        .and_then(Value::as_array)
        .expect("shims must be an array");

    for shim in shims {
        let name = shim
            .get("name")
            .and_then(Value::as_str)
            .expect("every shim entry needs a string name");
        assert_name_shape(name, "shims");
        let target = shim
            .get("target")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        assert!(
            package_names.contains(&target),
            "shim {name:?} targets {target:?}, which is not in packages"
        );
        assert_ne!(
            name, target,
            "shim {name:?} would shadow its own target {target:?}"
        );
        assert!(
            !package_names.contains(&name.to_string()),
            "shim {name:?} collides with a package of the same name"
        );
        assert!(
            shim.get("reason")
                .and_then(Value::as_str)
                .is_some_and(|reason| !reason.trim().is_empty()),
            "shim {name:?} needs a non-empty reason (why the extra hardlink exists)"
        );
    }
}

#[test]
fn excluded_records_owner_policy_and_never_reappears() {
    let manifest = load_manifest();
    let package_names = string_list_of(&manifest, "packages");
    let shim_names = string_list_of(&manifest, "shims");

    let exclusions = manifest
        .get("excluded")
        .and_then(Value::as_array)
        .expect("excluded must be an array");
    let mut excluded_names = Vec::new();
    for entry in exclusions {
        let name = entry
            .get("name")
            .and_then(Value::as_str)
            .expect("every excluded entry needs a string name");
        assert_name_shape(name, "excluded");
        assert!(
            entry
                .get("reason")
                .and_then(Value::as_str)
                .is_some_and(|reason| !reason.trim().is_empty()),
            "excluded {name:?} needs a non-empty reason (the exclusion IS the policy)"
        );
        excluded_names.push(name.to_string());
    }

    for name in &package_names {
        assert!(
            !excluded_names.contains(name),
            "package {name:?} is simultaneously excluded and installed"
        );
    }
    for name in &shim_names {
        assert!(
            !excluded_names.contains(name),
            "shim {name:?} is simultaneously excluded and materialized"
        );
    }

    // Owner-ordered exclusions must stay recorded even if the package list
    // never changes: compression tools (users wpm-install them), goawk
    // (gawk only), and `link` (MSVC link.exe collision — forbidden forever).
    for forbidden in ["bzip2", "gzip", "goawk", "link"] {
        assert!(
            excluded_names
                .iter()
                .any(|entry| entry.as_str() == forbidden),
            "owner exclusion {forbidden:?} vanished from the manifest"
        );
        assert!(
            !package_names.contains(&forbidden.to_string()),
            "forbidden package {forbidden:?} re-entered packages"
        );
        assert!(
            !shim_names.contains(&forbidden.to_string()),
            "forbidden name {forbidden:?} re-entered shims"
        );
    }
    // link is forbidden forever — its reason must say why, so a future
    // editor cannot mistake it for a stylistic exclusion.
    let link_reason = exclusions
        .iter()
        .find_map(|entry| {
            let name = entry.get("name").and_then(Value::as_str)?;
            (name == "link")
                .then(|| entry.get("reason").and_then(Value::as_str))
                .flatten()
        })
        .expect("link exclusion must exist");
    assert!(
        link_reason.to_ascii_lowercase().contains("msvc"),
        "the link exclusion reason must name the MSVC link.exe collision"
    );
}

#[test]
fn fail_open_disclosure_is_part_of_the_manifest() {
    // The fail-open contract is a property of the manifest, not of the
    // workflow: the notes must keep telling release editors that a failed
    // install warns and continues.
    let manifest = load_manifest();
    let notes = string_list(&manifest.get("notes").cloned().unwrap_or(Value::Null));
    assert!(
        notes
            .iter()
            .any(|note| note.to_ascii_lowercase().contains("fail-open")),
        "manifest notes must record the fail-open contract"
    );
    assert!(
        manifest
            .get("notes")
            .and_then(Value::as_array)
            .is_some_and(|notes| !notes.is_empty()),
        "manifest notes must not be empty"
    );
}

fn string_list_of(manifest: &Value, key: &str) -> Vec<String> {
    manifest
        .get(key)
        .and_then(Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .filter_map(|entry| entry.get("name").and_then(Value::as_str))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}
