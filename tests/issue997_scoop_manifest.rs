//! Static guard over the reference Scoop manifest (`installer/scoop/niubash.json`,
//! niubash#997).
//!
//! The manifest is the in-repo copy of the one submitted to the Scoop Extras
//! bucket (ScoopInstaller/Extras#19334). Scoop's `checkver` + `autoupdate`
//! rewrite it from the release tag alone, so it only keeps working while the
//! release workflow keeps publishing the names the templates expect. These
//! tests tie the templates to the two producers of those names instead of to
//! a hand-maintained copy:
//!
//! - `.github/workflows/release.yml` publishes the stable alias
//!   `niubash-win-<arch>.zip` for every Windows matrix arch;
//! - `scripts/package-release.ps1` stages the archive under the top-level
//!   directory `niubash-v<version>-win-<arch>` with `niu.exe` at its root.
//!
//! The concrete `version`/`url`/`hash` fields are release data (the hash is
//! only known after the release is built), so they are checked for shape and
//! self-consistency, not pinned to the crate version.

use std::fs;
use std::path::PathBuf;

use serde_json::Value;

const REPO_URL: &str = "https://github.com/unixwin/niubash";
/// Scoop architecture key -> the release matrix arch it maps to.
const ARCHES: [(&str, &str); 2] = [("64bit", "x64"), ("arm64", "arm64")];

fn repo_file(relative: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(relative);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

fn manifest() -> Value {
    serde_json::from_str(&repo_file("installer/scoop/niubash.json"))
        .expect("installer/scoop/niubash.json must be valid JSON")
}

fn str_at<'a>(value: &'a Value, pointer: &str) -> &'a str {
    value
        .pointer(pointer)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("manifest is missing string field {pointer}"))
}

fn alias_zip_template(arch: &str) -> String {
    format!("{REPO_URL}/releases/download/v$version/niubash-win-{arch}.zip")
}

fn extract_dir_template(arch: &str) -> String {
    format!("niubash-v$version-win-{arch}")
}

fn version_triple(version: &str) -> (u64, u64, u64) {
    let mut parts = version.split('.').map(|p| {
        p.parse::<u64>()
            .unwrap_or_else(|_| panic!("non-numeric version component in {version:?}"))
    });
    let triple = (
        parts.next().expect("major"),
        parts.next().expect("minor"),
        parts.next().expect("patch"),
    );
    assert!(
        parts.next().is_none(),
        "unexpected 4th component in {version:?}"
    );
    triple
}

#[test]
fn autoupdate_templates_follow_the_release_asset_names() {
    let m = manifest();
    for (scoop_arch, arch) in ARCHES {
        let base = format!("/autoupdate/architecture/{scoop_arch}");
        assert_eq!(str_at(&m, &format!("{base}/url")), alias_zip_template(arch));
        assert_eq!(
            str_at(&m, &format!("{base}/extract_dir")),
            extract_dir_template(arch)
        );
        // Github-mode checkver derives the hash from the release's asset
        // digests; a pinned autoupdate hash would go stale on the next tag.
        assert!(
            m.pointer(&format!("{base}/hash")).is_none(),
            "autoupdate must not pin a hash for {scoop_arch}"
        );
    }
    assert_eq!(str_at(&m, "/checkver/github"), REPO_URL);
    assert_eq!(str_at(&m, "/homepage"), REPO_URL);
}

#[test]
fn concrete_fields_are_the_templates_at_the_manifest_version() {
    let m = manifest();
    let version = str_at(&m, "/version");
    version_triple(version);
    for (scoop_arch, arch) in ARCHES {
        let base = format!("/architecture/{scoop_arch}");
        assert_eq!(
            str_at(&m, &format!("{base}/url")),
            alias_zip_template(arch).replace("$version", version)
        );
        assert_eq!(
            str_at(&m, &format!("{base}/extract_dir")),
            extract_dir_template(arch).replace("$version", version)
        );
        let hash = str_at(&m, &format!("{base}/hash"));
        assert!(
            hash.len() == 64
                && hash
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
            "{scoop_arch} hash must be a lowercase SHA-256 hex digest, got {hash:?}"
        );
    }
}

#[test]
fn manifest_version_is_never_ahead_of_the_crate() {
    let m = manifest();
    let manifest_version = version_triple(str_at(&m, "/version"));
    let crate_version = version_triple(env!("CARGO_PKG_VERSION"));
    assert!(
        manifest_version <= crate_version,
        "the reference manifest names a release ({manifest_version:?}) newer than the crate ({crate_version:?})"
    );
}

#[test]
fn release_workflow_publishes_the_alias_zip_for_every_manifest_arch() {
    let workflow = repo_file(".github/workflows/release.yml");
    assert!(
        workflow.contains(r#""dist/niubash-win-${{ matrix.arch }}.zip""#),
        "release.yml no longer publishes the niubash-win-<arch>.zip alias the Scoop autoupdate URL uses"
    );
    for (_, arch) in ARCHES {
        assert!(
            workflow.contains(&format!("- arch: {arch}")),
            "release.yml has no Windows matrix leg for {arch}"
        );
    }
}

#[test]
fn package_layout_matches_extract_dir_and_bin() {
    let script = repo_file("scripts/package-release.ps1");
    assert!(
        script.contains(r#"$packageName = "niubash-v$Version-win-$Arch""#),
        "package-release.ps1 changed the archive's top-level directory; update extract_dir"
    );
    assert!(
        script.contains(r#"(Join-Path $stageDir "niu.exe")"#),
        "package-release.ps1 no longer stages niu.exe at the archive root; update bin"
    );
    assert_eq!(str_at(&manifest(), "/bin"), "niu.exe");
}
