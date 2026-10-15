//! Manager descriptors — the plugin-manager open set as *data*.
//!
//! Design: `docs/planning/oh-my-niu-ecosystem.md` §14.6.2. Before this
//! module every manager was a hand-written adapter struct (detect /
//! list_assets / loader_snippet each re-implemented in code), so adding a
//! manager meant writing a new struct — the whack-a-mole shape (rubash#117)
//! one dimension over. The table below replaces those structs: **adding a
//! manager is one row of data, zero interpreter code** — the bpkg row in
//! this table is the proof.
//!
//! Two adapter kinds live here:
//!
//! * [`DescriptorAdapter`] — interprets one [`ManagerDescriptor`] row
//!   (oh-my-bash, bash-it, bash-completion, bpkg). One install per machine
//!   (source id = adapter id).
//! * [`FileSourceAdapter`] — the open fallback for *any* sourceable bash
//!   tree (a wild repo, a single-file plugin, a gist-style `.bash`). One
//!   install per plugin (source id derived from the origin). Assets are
//!   enumerated *honestly*: every `*.sh` / `*.bash` file is listed as a
//!   candidate with an annotation tag; niu never guesses a "unique entry",
//!   and enabling is a per-file guarded source (§14.4: identical to
//!   manually sourcing the file under GNU bash, missing functions and all).

use std::fs;
use std::path::Path;

use super::sources::{
    PluginSourceAdapter, SelectionModel, SourceAsset, SourceAssetKind, SourceRecord,
};

/// How a manager's assets are selected — the static half of the runtime
/// [`SelectionModel`].
#[derive(Debug, Clone, Copy)]
pub enum SelectionSpec {
    /// The manager's loader consumes rc arrays plus a theme variable.
    LoaderArrays {
        arrays: &'static [(&'static str, SourceAssetKind)],
        theme_var: &'static str,
    },
    /// The manager keeps its own `enabled/` directory (bash-it).
    EnabledDir { theme_var: &'static str },
    /// Activation is whole-source (bash-completion).
    WholeSource,
    /// Files are sourced individually (wild file sources, bpkg packages).
    DirectFiles,
}

/// How a manager's tree is recognized (layout fingerprint — §11.2: a
/// fingerprint, not a filename blacklist).
#[derive(Debug, Clone, Copy)]
pub enum LayoutFingerprint {
    /// Every path in `all` is a file, and at least one path in `any` is a
    /// file (`any` empty = no extra requirement).
    Files {
        all: &'static [&'static str],
        any: &'static [&'static str],
    },
    /// A JSON manifest at `path` (or `alt`) whose `scripts` field is an
    /// *array* of strings — bpkg's manifest shape. npm's `package.json`
    /// keeps `scripts` as an object, so plain npm trees never match.
    BpkgManifest,
}

/// One asset-enumeration pattern. Both manager families (oh-my-bash,
/// bash-it) enumerate with just these two shapes.
#[derive(Debug, Clone, Copy)]
pub enum AssetPattern {
    /// `<dir>/<name>/<name><suffix>` — directory-shaped asset
    /// (`themes/agnoster/agnoster.theme.sh`); the directory name is the
    /// asset name.
    DirNamed {
        dir: &'static str,
        suffix: &'static str,
        kind: SourceAssetKind,
    },
    /// `<dir>/<name><suffix>` — flat files, optionally under an
    /// `available/` subdir (`aliases/cargo.aliases.sh`,
    /// `plugins/available/git.plugin.bash`); the stripped remainder is the
    /// asset name.
    Flat {
        dir: &'static str,
        in_available: bool,
        suffixes: &'static [&'static str],
        kind: SourceAssetKind,
    },
    /// Files listed in the manifest's `scripts` array (bpkg); the listed
    /// relative path is the asset name.
    ManifestScripts,
}

/// How the guarded rc loader activates the source.
#[derive(Debug, Clone, Copy)]
pub enum LoaderSpec {
    /// Source one entry file after exporting a root variable (and optional
    /// extra exports). `root_var = None` sources the entry by path only.
    Entry {
        root_var: Option<&'static str>,
        entry: &'static str,
        extra_exports: &'static [(&'static str, &'static str)],
    },
    /// No block-level loader: activation is per-file source lines
    /// (`SelectionSpec::DirectFiles`); the loader snippet is empty.
    DirectFiles,
}

/// Static description of one known plugin manager. Everything niubash does
/// with the manager derives from this row.
pub struct ManagerDescriptor {
    /// Stable adapter id == source id (one install per manager per machine).
    pub id: &'static str,
    pub display_name: &'static str,
    /// License of the fetched tree as stated by the project.
    pub license: &'static str,
    /// Canonical git origin (`None` = local-path installs only; the user's
    /// own tooling downloads — bpkg per appendix A WP-S3).
    pub origin: Option<&'static str>,
    pub summary: &'static str,
    /// Extra note for `discover` when `origin` is `None` (how the user is
    /// expected to obtain the tree).
    pub install_note: Option<&'static str>,
    /// §14.6.2 id scope: `false` = manager installs one-per-machine (source
    /// id = manager id); `true` = per-package installs (id derives from the
    /// origin tail — adopted bpkg trees behave like wild file sources).
    pub per_install_id: bool,
    pub fingerprint: LayoutFingerprint,
    pub assets: &'static [AssetPattern],
    pub loader: LoaderSpec,
    pub selection: SelectionSpec,
    /// Loader-comment tail describing what stays active when the tree is
    /// absent (iron law 2 made explicit in the rc block).
    pub fallback_note: &'static str,
}

/// The manager table. Order matters for detection (flagship first); the
/// bpkg row demonstrates the §14.6.2 invariant — a whole manager as data.
pub const MANAGER_DESCRIPTORS: &[ManagerDescriptor] = &[
    // oh-my-bash — layout verified against the corpus checkout at
    // D:/repo/rubash/target-ecosys/repos/oh-my-bash.
    ManagerDescriptor {
        id: "oh-my-bash",
        display_name: "oh-my-bash",
        license: "MIT",
        origin: Some("https://github.com/ohmybash/oh-my-bash.git"),
        summary: "bash framework: 80+ themes, plugins and aliases",
        install_note: None,
        per_install_id: false,
        fingerprint: LayoutFingerprint::Files {
            all: &["oh-my-bash.sh"],
            any: &[],
        },
        assets: &[
            AssetPattern::DirNamed {
                dir: "themes",
                suffix: ".theme.sh",
                kind: SourceAssetKind::Theme,
            },
            // Defensive flat fallback: themes/<name>.theme.sh.
            AssetPattern::Flat {
                dir: "themes",
                in_available: false,
                suffixes: &[".theme.sh"],
                kind: SourceAssetKind::Theme,
            },
            AssetPattern::DirNamed {
                dir: "plugins",
                suffix: ".plugin.sh",
                kind: SourceAssetKind::Plugin,
            },
            AssetPattern::Flat {
                dir: "aliases",
                in_available: false,
                suffixes: &[".aliases.sh", ".aliases.bash"],
                kind: SourceAssetKind::Alias,
            },
            AssetPattern::Flat {
                dir: "completions",
                in_available: false,
                suffixes: &[".completion.sh", ".completion.bash"],
                kind: SourceAssetKind::Completion,
            },
        ],
        loader: LoaderSpec::Entry {
            root_var: Some("OSH"),
            entry: "oh-my-bash.sh",
            // §3.3: mark the theme channel for the bash-compatible PS1
            // renderer; the theme itself comes from the managed block.
            extra_exports: &[("NIU_THEME_SOURCE", "omb")],
        },
        selection: SelectionSpec::LoaderArrays {
            arrays: &[
                ("plugins", SourceAssetKind::Plugin),
                ("aliases", SourceAssetKind::Alias),
                ("completions", SourceAssetKind::Completion),
            ],
            theme_var: "OSH_THEME",
        },
        fallback_note: "fallback stays active when absent",
    },
    // bash-it — layout verified against a fresh checkout of upstream main
    // (2026-10-02, target/upstream-audit/bash-it). Fingerprint lesson from
    // the 1.3.0 dead end: upstream moved the vendored composure library out
    // of `lib/composure.bash` (present in 2.x releases, gone from main,
    // which carries `lib/utilities.bash` instead), so a fingerprint naming
    // BOTH markers as required rejected the real tree and the startup
    // bootstrap retried the 1.36MB clone every terminal. The root loader
    // stays mandatory; the lib marker accepts either generation (plus the
    // singular `completion/` dir below — not OMB's plural `completions/`).
    ManagerDescriptor {
        id: "bash-it",
        display_name: "bash-it",
        license: "MIT",
        origin: Some("https://github.com/Bash-it/bash-it.git"),
        summary: "bash framework: community aliases, plugins, completions, themes",
        install_note: None,
        per_install_id: false,
        fingerprint: LayoutFingerprint::Files {
            all: &["bash_it.sh"],
            any: &["lib/composure.bash", "lib/utilities.bash"],
        },
        assets: &[
            AssetPattern::Flat {
                dir: "aliases",
                in_available: true,
                suffixes: &[".aliases.bash", ".plugin.bash", ".completion.bash"],
                kind: SourceAssetKind::Alias,
            },
            AssetPattern::Flat {
                dir: "plugins",
                in_available: true,
                suffixes: &[".aliases.bash", ".plugin.bash", ".completion.bash"],
                kind: SourceAssetKind::Plugin,
            },
            AssetPattern::Flat {
                dir: "completion",
                in_available: true,
                suffixes: &[".aliases.bash", ".plugin.bash", ".completion.bash"],
                kind: SourceAssetKind::Completion,
            },
            AssetPattern::DirNamed {
                dir: "themes",
                suffix: ".theme.bash",
                kind: SourceAssetKind::Theme,
            },
        ],
        loader: LoaderSpec::Entry {
            root_var: Some("BASH_IT"),
            entry: "bash_it.sh",
            extra_exports: &[],
        },
        selection: SelectionSpec::EnabledDir {
            theme_var: "BASH_IT_THEME",
        },
        fallback_note: "fallback stays active when absent",
    },
    // bash-completion — design §4.1 as a source; GPL, fetch-on-demand only.
    ManagerDescriptor {
        id: "bash-completion",
        display_name: "bash-completion",
        license: "GPL-2.0-or-later",
        origin: Some("https://github.com/scop/bash-completion.git"),
        summary: "the standard bash completion library (fetched on demand; GPL-2.0+)",
        install_note: None,
        per_install_id: false,
        fingerprint: LayoutFingerprint::Files {
            all: &["bash_completion"],
            any: &[],
        },
        assets: &[AssetPattern::Flat {
            dir: "completions",
            in_available: false,
            suffixes: &[".sh", ".bash"],
            kind: SourceAssetKind::Completion,
        }],
        loader: LoaderSpec::Entry {
            root_var: None,
            entry: "bash_completion",
            extra_exports: &[],
        },
        selection: SelectionSpec::WholeSource,
        fallback_note: "native completions stay as fallback",
    },
    // bpkg — appendix A WP-S3, "drive bpkg's own CLI": niu never downloads
    // bpkg packages itself (`origin = None`); the user runs
    // `bpkg install <pkg>` and adopts the tree with `--path`. niu only
    // takes over loading (source the manifest's scripts). Per-package ids
    // (`per_install_id = true`): each adopted package is its own source,
    // id derived from the tree/origin name.
    ManagerDescriptor {
        id: "bpkg",
        display_name: "bpkg package",
        license: "varies (per package)",
        origin: None,
        summary: "bash package managed by bpkg (niu adopts an installed tree)",
        install_note: Some(
            "install with bpkg itself (`bpkg install <user/repo>`), \
             then adopt the tree with `niu plugin add bpkg --path <dir>`",
        ),
        per_install_id: true,
        fingerprint: LayoutFingerprint::BpkgManifest,
        assets: &[AssetPattern::ManifestScripts],
        loader: LoaderSpec::DirectFiles,
        selection: SelectionSpec::DirectFiles,
        fallback_note: "fallback stays active when absent",
    },
];

/// Adapter backed by one [`ManagerDescriptor`] row.
pub struct DescriptorAdapter(pub &'static ManagerDescriptor);

impl super::sources::PluginSourceAdapter for DescriptorAdapter {
    fn id(&self) -> &'static str {
        self.0.id
    }
    fn display_name(&self) -> &'static str {
        self.0.display_name
    }
    fn license(&self) -> &'static str {
        self.0.license
    }
    fn default_origin(&self) -> Option<&'static str> {
        self.0.origin
    }
    fn summary(&self) -> &'static str {
        self.0.summary
    }
    fn install_note(&self) -> Option<&'static str> {
        self.0.install_note
    }
    fn per_install_id(&self) -> bool {
        self.0.per_install_id
    }
    fn detect(&self, root: &Path) -> bool {
        match self.0.fingerprint {
            LayoutFingerprint::Files { all, any } => {
                all.iter().all(|path| root.join(path).is_file())
                    && (any.is_empty() || any.iter().any(|path| root.join(path).is_file()))
            }
            LayoutFingerprint::BpkgManifest => bpkg_manifest_scripts(root).is_some(),
        }
    }
    fn installed_version(&self, root: &Path) -> String {
        super::sources::git_head_short_sha(root).unwrap_or_else(|| "unknown".to_string())
    }
    fn list_assets(&self, root: &Path) -> Vec<SourceAsset> {
        let mut assets = Vec::new();
        for pattern in self.0.assets {
            match *pattern {
                AssetPattern::DirNamed { dir, suffix, kind } => {
                    enumerate_dir_named(root, dir, suffix, kind, &mut assets);
                }
                AssetPattern::Flat {
                    dir,
                    in_available,
                    suffixes,
                    kind,
                } => {
                    let base = if in_available {
                        root.join(dir).join("available")
                    } else {
                        root.join(dir)
                    };
                    enumerate_flat(&base, suffixes, kind, &mut assets);
                }
                AssetPattern::ManifestScripts => {
                    if let Some(scripts) = bpkg_manifest_scripts(root) {
                        for script in scripts {
                            let path = root.join(&script);
                            if path.is_file() {
                                assets.push(SourceAsset {
                                    kind: SourceAssetKind::Plugin,
                                    name: script,
                                    path,
                                    tag: Some("bpkg script".to_string()),
                                });
                            }
                        }
                    }
                }
            }
        }
        assets.sort_by(|left, right| {
            (left.kind.as_str(), &left.name).cmp(&(right.kind.as_str(), &right.name))
        });
        assets
    }
    fn loader_snippet(&self, record: &SourceRecord) -> String {
        let base = sources_base(record);
        match self.0.loader {
            LoaderSpec::Entry {
                root_var,
                entry,
                extra_exports,
            } => {
                let mut snippet = String::new();
                snippet.push_str(&format!(
                    "# {id} source (external, primary; {note})\n",
                    id = record.id,
                    note = self.0.fallback_note,
                ));
                snippet.push_str("if [ -f \"");
                snippet.push_str(&base);
                snippet.push('/');
                snippet.push_str(entry);
                snippet.push_str("\" ]; then\n");
                if let Some(var) = root_var {
                    snippet.push_str(&format!("  {var}=\"{base}\"\n"));
                    // Separator normalization keeps in-shell globs working
                    // when NIU_PLUGIN_SOURCES_ROOT is in Windows form (the
                    // closed `${var//\\//}` substitution, same form as the
                    // rc HOME bootstrap).
                    snippet.push_str("  ");
                    snippet.push_str(var);
                    snippet.push_str("=\"${");
                    snippet.push_str(var);
                    snippet.push_str("//\\\\//}\"\n");
                    snippet.push_str(&format!("  export {var}\n"));
                }
                for (key, value) in extra_exports {
                    snippet.push_str(&format!("  export {key}={value}\n"));
                }
                match root_var {
                    Some(var) => {
                        snippet.push_str("  . \"$");
                        snippet.push_str(var);
                        snippet.push('/');
                        snippet.push_str(entry);
                        snippet.push_str("\"\n");
                    }
                    None => {
                        snippet.push_str("  . \"");
                        snippet.push_str(&base);
                        snippet.push('/');
                        snippet.push_str(entry);
                        snippet.push_str("\"\n");
                    }
                }
                snippet.push_str("fi\n");
                snippet
            }
            LoaderSpec::DirectFiles => String::new(),
        }
    }

    fn selection_model(&self) -> SelectionModel {
        match self.0.selection {
            SelectionSpec::LoaderArrays { arrays, theme_var } => {
                SelectionModel::LoaderArrays { arrays, theme_var }
            }
            SelectionSpec::EnabledDir { theme_var } => SelectionModel::EnabledDir { theme_var },
            SelectionSpec::WholeSource => SelectionModel::WholeSource,
            SelectionSpec::DirectFiles => SelectionModel::DirectFiles,
        }
    }
}

/// The open fallback: any tree with sourceable bash files. Adapter id
/// `"file"` — records carry a per-install id instead (one source per
/// plugin, not per machine).
pub struct FileSourceAdapter;

impl super::sources::PluginSourceAdapter for FileSourceAdapter {
    fn id(&self) -> &'static str {
        "file"
    }
    fn display_name(&self) -> &'static str {
        "standalone bash files"
    }
    fn license(&self) -> &'static str {
        "unknown (review before trust)"
    }
    fn default_origin(&self) -> Option<&'static str> {
        None
    }
    fn summary(&self) -> &'static str {
        "any sourceable bash files (candidates listed, you pick)"
    }
    fn install_note(&self) -> Option<&'static str> {
        Some("candidates are listed honestly — pick files with `niu plugin enable`")
    }
    fn per_install_id(&self) -> bool {
        true
    }
    fn detect(&self, root: &Path) -> bool {
        !enumerate_wild_candidates(root).is_empty()
    }
    fn installed_version(&self, root: &Path) -> String {
        super::sources::git_head_short_sha(root).unwrap_or_else(|| "snapshot".to_string())
    }
    fn list_assets(&self, root: &Path) -> Vec<SourceAsset> {
        enumerate_wild_candidates(root)
    }
    fn loader_snippet(&self, _record: &SourceRecord) -> String {
        String::new() // per-file guarded source lines (DirectFiles)
    }
    fn selection_model(&self) -> SelectionModel {
        SelectionModel::DirectFiles
    }
}

/// Maximum wild candidates enumerated per tree (honest cap, far above any
/// sane plugin repo; keeps pathological trees bounded).
const WILD_CANDIDATE_CAP: usize = 256;

/// Enumerate every `*.sh` / `*.bash` file under `root` (skipping `.git`),
/// shallowest paths first, each tagged for honest presentation. Nothing is
/// filtered out — `install.sh` and test scripts are *listed with a tag*,
/// never silently hidden and never guessed to be "the" entry (§14.6.1).
pub fn enumerate_wild_candidates(root: &Path) -> Vec<SourceAsset> {
    let mut files: Vec<(usize, String)> = Vec::new(); // (depth, posix rel path)
    collect_shell_files(root, root, 0, &mut files);
    files.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
    files
        .into_iter()
        .take(WILD_CANDIDATE_CAP)
        .map(|(_, relative)| {
            let path = root.join(relative.replace('/', std::path::MAIN_SEPARATOR_STR));
            let kind = classify_wild_file(&path);
            let tag = wild_tag(&name_for_tag(&path), &path);
            SourceAsset {
                kind,
                name: relative,
                path,
                tag: Some(tag),
            }
        })
        .collect()
}

fn name_for_tag(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default()
        .to_string()
}

fn wild_tag(file_name: &str, path: &Path) -> String {
    let installer_like = matches!(
        file_name.to_ascii_lowercase().as_str(),
        "install.sh" | "setup.sh" | "uninstall.sh" | "bootstrap.sh"
    ) || path.components().any(|component| {
        matches!(
            component.as_os_str().to_str(),
            Some("tests") | Some("test") | Some("spec")
        )
    });
    if installer_like {
        "installer/test-like — review before sourcing".to_string()
    } else if has_shebang(path) {
        "script".to_string()
    } else {
        "fragment".to_string()
    }
}

/// Cheap semantic classification (not entry-guessing): a file that
/// registers completions is surfaced as a completion asset.
fn classify_wild_file(path: &Path) -> SourceAssetKind {
    if fs::read_to_string(path)
        .map(|text| text.contains("complete ") && text.contains(" -F"))
        .unwrap_or(false)
    {
        SourceAssetKind::Completion
    } else {
        SourceAssetKind::Plugin
    }
}

fn has_shebang(path: &Path) -> bool {
    fs::read_to_string(path)
        .ok()
        .and_then(|text| text.lines().next().map(|line| line.starts_with("#!")))
        .unwrap_or(false)
}

fn collect_shell_files(root: &Path, dir: &Path, depth: usize, out: &mut Vec<(usize, String)>) {
    if out.len() >= WILD_CANDIDATE_CAP {
        return;
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.filter_map(Result::ok) {
        if out.len() >= WILD_CANDIDATE_CAP {
            return;
        }
        let path = entry.path();
        let Some(name) = entry.file_name().into_string().ok() else {
            continue;
        };
        if name == ".git" {
            continue;
        }
        if path.is_dir() {
            collect_shell_files(root, &path, depth + 1, out);
        } else if path.is_file() {
            let extension = path.extension().and_then(|e| e.to_str());
            if extension == Some("sh") || extension == Some("bash") {
                if let Ok(relative) = path.strip_prefix(root) {
                    out.push((depth, relative.to_string_lossy().replace('\\', "/")));
                }
            }
        }
    }
}

/// Enumerate `<dir>/<name>/<name><suffix>` (directory-shaped assets).
fn enumerate_dir_named(
    root: &Path,
    dir: &'static str,
    suffix: &'static str,
    kind: SourceAssetKind,
    out: &mut Vec<SourceAsset>,
) {
    let Ok(entries) = fs::read_dir(root.join(dir)) else {
        return;
    };
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let script = path.join(format!("{name}{suffix}"));
        if script.is_file() {
            out.push(SourceAsset {
                kind,
                name: name.to_string(),
                path: script,
                tag: None,
            });
        }
    }
}

/// Enumerate flat `<name><suffix>` files in one directory.
fn enumerate_flat(
    dir: &Path,
    suffixes: &'static [&'static str],
    kind: SourceAssetKind,
    out: &mut Vec<SourceAsset>,
) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some(file) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let Some(name) = suffixes.iter().find_map(|suffix| file.strip_suffix(suffix)) else {
            continue;
        };
        if !name.is_empty() {
            out.push(SourceAsset {
                kind,
                name: name.to_string(),
                path: path.clone(),
                tag: None,
            });
        }
    }
}

/// Read the bpkg manifest's `scripts` array (`bpkg.json`, or a
/// `package.json` whose `scripts` is an array — npm's object shape is
/// rejected). Returns the declared script paths.
fn bpkg_manifest_scripts(root: &Path) -> Option<Vec<String>> {
    for manifest in ["bpkg.json", "package.json"] {
        let path = root.join(manifest);
        let Ok(text) = fs::read_to_string(&path) else {
            continue;
        };
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
            continue;
        };
        if let Some(scripts) = value.get("scripts").and_then(|s| s.as_array()) {
            let files: Vec<String> = scripts
                .iter()
                .filter_map(|script| script.as_str().map(str::to_string))
                .collect();
            if files.len() == scripts.len() && !files.is_empty() {
                return Some(files);
            }
        }
    }
    None
}

/// The rc-facing base path of a source tree (`$HOME/.niubash/sources/<id>`
/// with the standard override chain).
pub fn sources_base(record: &SourceRecord) -> String {
    format!(
        "${{NIU_PLUGIN_SOURCES_ROOT:-$HOME/.niubash/sources}}/{}",
        record.id
    )
}

/// Manager adapters only — the list `discover`/`clean`/catalog iterate.
/// The wild file-source fallback is deliberately excluded: `clean` must
/// never treat an arbitrary directory containing shell scripts as an
/// orphaned manager tree (§14.6.2).
static OMB_ADAPTER: DescriptorAdapter = DescriptorAdapter(&MANAGER_DESCRIPTORS[0]);
static BASH_IT_ADAPTER: DescriptorAdapter = DescriptorAdapter(&MANAGER_DESCRIPTORS[1]);
static BASH_COMPLETION_ADAPTER: DescriptorAdapter = DescriptorAdapter(&MANAGER_DESCRIPTORS[2]);
static BPKG_ADAPTER: DescriptorAdapter = DescriptorAdapter(&MANAGER_DESCRIPTORS[3]);
static FILE_SOURCE_ADAPTER: FileSourceAdapter = FileSourceAdapter;

static MANAGER_ADAPTERS: [&'static dyn PluginSourceAdapter; 4] = [
    &OMB_ADAPTER,
    &BASH_IT_ADAPTER,
    &BASH_COMPLETION_ADAPTER,
    &BPKG_ADAPTER,
];

pub fn builtin_source_adapters() -> &'static [&'static dyn PluginSourceAdapter] {
    &MANAGER_ADAPTERS
}

/// Look up an adapter by kind id (`"oh-my-bash"` … or `"file"`).
pub fn adapter_for(kind: &str) -> Option<&'static dyn PluginSourceAdapter> {
    if kind == "file" {
        return Some(&FILE_SOURCE_ADAPTER);
    }
    builtin_source_adapters()
        .iter()
        .copied()
        .find(|adapter| adapter.id() == kind)
}

/// Detect the owning manager by layout fingerprint (managers only — no
/// wild fallback; used by `clean` for orphan detection).
pub fn detect_source_adapter(root: &Path) -> Option<&'static dyn PluginSourceAdapter> {
    builtin_source_adapters()
        .iter()
        .copied()
        .find(|adapter| adapter.detect(root))
}

/// Detect the adapter for an *install*: any known manager first, then the
/// open file-source fallback (a tree with at least one sourceable
/// `*.sh`/`*.bash`). `None` means the tree offers nothing niubash could
/// honestly source.
pub fn detect_adapter_for_install(root: &Path) -> Option<&'static dyn PluginSourceAdapter> {
    if let Some(manager) = detect_source_adapter(root) {
        return Some(manager);
    }
    FILE_SOURCE_ADAPTER
        .detect(root)
        .then_some(&FILE_SOURCE_ADAPTER)
}

/// Derive the install id for a fetched source: an explicit id wins; a
/// one-per-machine manager uses the manager id; per-install shapes (wild
/// file sources, adopted bpkg trees — `per_install_id()`) derive the id
/// from the origin tail (repo / directory name) so each package is its own
/// source.
pub fn derive_install_id(
    adapter: &dyn PluginSourceAdapter,
    origin: &str,
    explicit: Option<&str>,
) -> anyhow::Result<String> {
    if let Some(explicit) = explicit {
        let id = sanitize_source_id(explicit)?;
        return Ok(id);
    }
    if !adapter.per_install_id() {
        return Ok(adapter.id().to_string());
    }
    let tail = origin_tail(origin);
    if tail.is_empty() {
        anyhow::bail!(
            "cannot derive a source id from origin '{origin}'; \
             pass an explicit one with --id <name>"
        );
    }
    sanitize_source_id(&tail)
}

/// Last meaningful path segment of an origin: URL tail (`.git` stripped)
/// or directory name.
fn origin_tail(origin: &str) -> String {
    let trimmed = origin.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        return String::new();
    }
    let tail = trimmed
        .rsplit('/')
        .next()
        .unwrap_or(trimmed)
        .trim_end_matches(".git")
        .to_string();
    // Windows path tail (`D:\x\y` or `D:/x/y`).
    let tail = tail.rsplit(['\\', '/']).next().unwrap_or(&tail).to_string();
    tail
}

fn sanitize_source_id(raw: &str) -> anyhow::Result<String> {
    let mut id = String::new();
    let mut last_dash = false;
    for ch in raw.chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
            id.push(ch);
            last_dash = false;
        } else if !last_dash {
            id.push('-');
            last_dash = true;
        }
    }
    let id = id.trim_matches(['-', '.']).to_string();
    if id.is_empty() {
        anyhow::bail!("source id '{raw}' has no usable characters");
    }
    Ok(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn origin_tail_extraction() {
        assert_eq!(
            origin_tail("https://github.com/rcaloras/bash-preexec.git"),
            "bash-preexec"
        );
        assert_eq!(origin_tail("https://example.com/x/y/"), "y");
        assert_eq!(origin_tail("D:/tmp/my plugin"), "my plugin");
        assert_eq!(origin_tail(""), "");
    }

    #[test]
    fn install_id_derivation() {
        let omb = adapter_for("oh-my-bash").unwrap();
        let bpkg = adapter_for("bpkg").unwrap();
        let file = adapter_for("file").unwrap();
        // One-per-machine managers keep the manager id.
        assert_eq!(
            derive_install_id(omb, "https://x/y.git", None).unwrap(),
            "oh-my-bash"
        );
        assert!(!omb.per_install_id());
        // Per-install shapes (wild file sources, adopted bpkg trees) derive
        // from the origin tail — one source per package.
        assert!(bpkg.per_install_id());
        assert_eq!(
            derive_install_id(bpkg, "https://github.com/a/bpkg-pkg.git", None).unwrap(),
            "bpkg-pkg"
        );
        assert_eq!(
            derive_install_id(file, "https://github.com/a/b.git", None).unwrap(),
            "b"
        );
        // Explicit ids win and are sanitized.
        assert_eq!(
            derive_install_id(file, "https://x/y.git", Some("My Plugin!")).unwrap(),
            "My-Plugin"
        );
        assert!(derive_install_id(file, "", None).is_err());
    }

    #[test]
    fn manager_table_is_the_catalog_backbone() {
        let ids: Vec<&str> = MANAGER_DESCRIPTORS.iter().map(|d| d.id).collect();
        assert_eq!(ids, ["oh-my-bash", "bash-it", "bash-completion", "bpkg"]);
        // Every row is complete enough to describe itself.
        for descriptor in MANAGER_DESCRIPTORS {
            assert!(!descriptor.summary.is_empty());
            assert!(!descriptor.license.is_empty());
        }
        // The local-install manager carries the note and the per-package id
        // scope; catalog managers do not need either.
        let bpkg = &MANAGER_DESCRIPTORS[3];
        assert!(bpkg.origin.is_none());
        assert!(bpkg.install_note.unwrap().contains("bpkg install"));
        assert!(bpkg.per_install_id);
        assert!(MANAGER_DESCRIPTORS[..3].iter().all(|d| !d.per_install_id));
    }

    /// Fingerprint honesty against the REAL upstream layouts (1.3.1 F6):
    /// bash-it's vendored composure library moved from
    /// `lib/composure.bash` (2.x releases) out of the tree — upstream main
    /// carries `lib/utilities.bash` instead, and 1.3.0's fingerprint
    /// required BOTH old markers, so a plain `niu plugin sync` of the real
    /// repo failed "does not look like 'bash-it'" and the startup
    /// bootstrap retried the full clone on every terminal. The fixture
    /// trees below mirror the marker files of the actual checkouts
    /// (verified against fresh clones, 2026-10-02); both generations must
    /// detect, and an OMB tree must still NOT match bash-it.
    #[test]
    fn bash_it_fingerprint_accepts_both_real_upstream_generations() {
        let adapter = adapter_for("bash-it").unwrap();

        // Current upstream main: bash_it.sh + lib/utilities.bash + the
        // singular completion/ dir (not OMB's plural completions/).
        let temp = std::env::temp_dir().join(format!(
            "niu-desc-bashit-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let modern = temp.join("modern");
        fs::create_dir_all(modern.join("lib")).unwrap();
        fs::create_dir_all(modern.join("completion/available")).unwrap();
        fs::create_dir_all(modern.join("aliases/available")).unwrap();
        fs::create_dir_all(modern.join("themes/bakke")).unwrap();
        fs::write(modern.join("bash_it.sh"), "# loader\n").unwrap();
        fs::write(modern.join("lib/utilities.bash"), "# utilities\n").unwrap();
        fs::write(
            modern.join("completion/available/docker.completion.bash"),
            "# docker\n",
        )
        .unwrap();
        fs::write(modern.join("aliases/available/apt.aliases.bash"), "# apt\n").unwrap();
        fs::write(
            modern.join("themes/bakke/bakke.theme.bash"),
            "PS1='bakke> '\n",
        )
        .unwrap();
        assert!(adapter.detect(&modern), "real main layout must detect");
        let assets = adapter.list_assets(&modern);
        let names: Vec<&str> = assets.iter().map(|a| a.name.as_str()).collect();
        assert!(names.contains(&"docker"), "completion asset: {names:?}");
        assert!(names.contains(&"apt"), "alias asset: {names:?}");
        assert!(names.contains(&"bakke"), "theme asset: {names:?}");

        // 2.x releases: composure.bash instead of utilities.bash.
        let legacy = temp.join("legacy");
        fs::create_dir_all(legacy.join("lib")).unwrap();
        fs::write(legacy.join("bash_it.sh"), "# loader\n").unwrap();
        fs::write(legacy.join("lib/composure.bash"), "# composure\n").unwrap();
        assert!(adapter.detect(&legacy), "2.x layout must still detect");

        // The root loader alone (no lib marker of either generation) does
        // not match — and neither does a full oh-my-bash tree.
        let bare = temp.join("bare");
        fs::create_dir_all(&bare).unwrap();
        fs::write(bare.join("bash_it.sh"), "# loader\n").unwrap();
        assert!(!adapter.detect(&bare), "loader alone is not a fingerprint");
        let omb = temp.join("omb");
        fs::create_dir_all(&omb).unwrap();
        fs::write(omb.join("oh-my-bash.sh"), "# omb\n").unwrap();
        assert!(!adapter.detect(&omb), "an OMB tree is not bash-it");

        let _ = fs::remove_dir_all(&temp);
    }
}
