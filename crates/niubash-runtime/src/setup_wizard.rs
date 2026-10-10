//! First-run setup wizard.
//!
//! Minimal external-first flow (niubash#145 owner decision 2026-09-28): the
//! built-in theme/plugin stack is retired, so the wizard writes a clean rc
//! and, when trusted external plugin-manager sources are installed, offers
//! their themes (oh-my-bash) through the bash-compatible PS1 channel. Every
//! question defaults to "no change", nothing is installed without an
//! explicit pick. Presets stay available non-interactively via
//! `niu setup --preset <name>` as alias-comfort levels.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
#[cfg(windows)]
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::interactive_menu::{self, pad_display, Selection};
use crate::path_utils::shell_home_dir;

const PRIMARY_RC_FILE: &str = ".niubashrc";
const COMPAT_RC_FILE: &str = ".winuxshrc";
const SETUP_DONE_FILE: &str = ".setup-done";

/// One-shot handoff marker (niubash#180): every rc rewrite leaves
/// `~/.niubash/setup-apply-pending` behind, and a live interactive session
/// consumes it at its next prompt to re-source the new rc in-process —
/// writing the configuration and applying it to the running session are no
/// longer two different verbs. The consumer lives in `shell.rs`
/// ([`crate::shell`] `apply_setup_config_if_pending`); a session that
/// sources the rc at startup consumes stale markers as its baseline, so
/// only a setup that ran while the session was live triggers the re-apply.
const APPLY_PENDING_FILE: &str = "setup-apply-pending";

/// Schema marker for the wizard answers file (`~/.niubash/wizard-answers.toml`).
const WIZARD_ANSWERS_SCHEMA: &str = "niubash:wizard-answers@0.1.0";

/// Schema marker for the setup journal (`~/.niubash/setup-journal.toml`).
const SETUP_JOURNAL_SCHEMA: &str = "niubash:setup-journal@0.1.0";

/// What one applied setup run changed — the durable side of the undo
/// contract (§0 iron law 3: every wizard write is reversible). The finish
/// screen prints one undo command per entry; `niu plugin rollback` /
/// `niu plugin source rollback` cover the bundle/source channels.
#[derive(Debug, Default)]
struct SetupJournal {
    /// Backup of the previous rc (when one existed).
    rc_backup: Option<PathBuf>,
    /// `true` when this run *created* the rc (fresh install: no previous
    /// rc existed, so there is no backup to restore). The undo receipt
    /// must then cover the write itself — remove the generated rc and the
    /// setup-done marker (niubash#179 L02-1).
    rc_created: bool,
    /// Theme picked from an external source: (name, source id).
    theme: Option<(String, String)>,
    /// Preset name when `niu setup --preset` produced this rc.
    preset: Option<String>,
    /// Lasting niu-git answer ("never" / "installed").
    niu_git: Option<String>,
    /// Plugin collection applied by this run: name + the source ids that
    /// landed, so the undo lines can name each one.
    collection: Option<CollectionJournal>,
}

/// The journalable part of a wizard collection apply.
#[derive(Debug, Default)]
struct CollectionJournal {
    name: String,
    /// Source ids that actually landed (undo targets).
    sources: Vec<String>,
    /// Entries that failed to install (recipe ids), plus a
    /// `"apply failed: <error>"` row when the whole apply errored — the
    /// journal tells the truth about partial and total failure alike
    /// (journey run-13 observation: a failed apply was journaled as a
    /// bare success line and the finish screen said nothing).
    failed: Vec<String>,
}

fn setup_journal_path(home: &std::path::Path) -> PathBuf {
    home.join(".niubash").join("setup-journal.toml")
}

/// Write the journal for the run that was just applied. Failures are
/// reported but never fail the wizard (the rc write already succeeded).
fn write_setup_journal(home: &std::path::Path, journal: &SetupJournal) {
    let path = setup_journal_path(home);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let mut body = format!(
        "# Applied setup run — the finish screen printed an undo command per entry.\n\
         schema = \"{SETUP_JOURNAL_SCHEMA}\"\n\
         applied_at = \"{}\"\n",
        timestamp_id()
    );
    if let Some(backup) = &journal.rc_backup {
        body.push_str(&format!(
            "rc_backup = {}\n",
            shell_quote(&backup.to_string_lossy())
        ));
    }
    if journal.rc_created {
        body.push_str("rc_created = true\n");
    }
    if let Some((theme, source)) = &journal.theme {
        body.push_str(&format!(
            "theme = {}\ntheme_source = {}\n",
            shell_quote(theme),
            shell_quote(source)
        ));
    }
    if let Some(preset) = &journal.preset {
        body.push_str(&format!("preset = {}\n", shell_quote(preset)));
    }
    if let Some(niu_git) = &journal.niu_git {
        body.push_str(&format!("niu_git = {}\n", shell_quote(niu_git)));
    }
    if let Some(collection) = &journal.collection {
        body.push_str(&format!("collection = {}\n", shell_quote(&collection.name)));
        if !collection.sources.is_empty() {
            let ids: Vec<String> = collection
                .sources
                .iter()
                .map(|id| shell_quote(id))
                .collect();
            body.push_str(&format!("collection_sources = [{}]\n", ids.join(", ")));
        }
        // Honesty (journey run-13): a collection whose entries failed — or
        // whose whole apply errored — must never read back as a success.
        if !collection.failed.is_empty() {
            let ids: Vec<String> = collection.failed.iter().map(|id| shell_quote(id)).collect();
            body.push_str(&format!("collection_failed = [{}]\n", ids.join(", ")));
        }
    }
    if let Err(err) = std::fs::write(&path, body) {
        println!(
            "  \u{26a0}\u{fe0f}  {} {err}",
            Lang::detect().tr("could not write the setup journal")
        );
    }
}

/// One undo command per journal entry (§6.3). A fresh install (niubash#179
/// L02-1) has no previous rc to restore, so its receipt covers the write
/// itself: remove the generated rc (guard block included) and the setup
/// marker. Source ids are deduplicated (L02-2): when the theme and the
/// collection name the same source, the removal line prints once.
fn setup_undo_lines(home: &std::path::Path, journal: &SetupJournal) -> Vec<String> {
    let mut lines = Vec::new();
    if let Some(backup) = &journal.rc_backup {
        lines.push(format!(
            "cp {} {}                  # restore the previous rc",
            backup.display(),
            home.join(PRIMARY_RC_FILE).display()
        ));
    }
    if journal.rc_created {
        lines.push(format!(
            "rm {}                  # fresh install: remove the generated rc",
            home.join(PRIMARY_RC_FILE).display()
        ));
        lines.push(format!(
            "rm {}                  # and the setup-done marker",
            home.join(".niubash").join(SETUP_DONE_FILE).display()
        ));
    }
    let mut named_sources: BTreeSet<&str> = BTreeSet::new();
    if let Some((theme, source)) = &journal.theme {
        lines.push(format!(
            "niu plugin disable {theme}          # drop the theme pick"
        ));
        if named_sources.insert(source.as_str()) {
            lines.push(format!(
                "niu plugin source remove {source:<12}  # optional: also delete the tree"
            ));
        }
    }
    if let Some(collection) = &journal.collection {
        for id in &collection.sources {
            // Deduplicated (niubash#179 L02-2): the theme and the collection
            // may name the same source; the removal receipt prints once.
            if named_sources.insert(id.as_str()) {
                lines.push(format!(
                    "niu plugin source remove {id:<12}  # collection '{}': drop the source",
                    collection.name
                ));
            }
        }
    }
    lines
}

/// One retry command per failed collection entry: the exact verb the apply
/// itself named mid-run (`niu plugin distro apply <name>` — verified verb,
/// wired in `run_plugin_distro_command`). Only the failed entries are
/// called out; a healthy apply prints nothing here.
fn setup_retry_lines(journal: &SetupJournal) -> Vec<String> {
    let Some(collection) = &journal.collection else {
        return Vec::new();
    };
    if collection.failed.is_empty() {
        return Vec::new();
    }
    vec![format!(
        "niu plugin distro apply {}     # collection '{}': {} entr{} failed",
        collection.name,
        collection.name,
        collection.failed.len(),
        if collection.failed.len() == 1 {
            "y"
        } else {
            "ies"
        }
    )]
}

/// niu-git recipe id in the compiled-in plugin recipe index. The wizard
/// only ever names this recipe; its install pick prints the recipe's
/// package-manager recommendation (wpm first — niu-git ships in the wpm
/// index — plus the upstream releases page). niu itself installs nothing
/// (download retraction, owner ruling 2026-10-04). Windows-only because
/// niu-git is Windows-native git (the topic and the question never appear
/// on other platforms).
#[cfg(windows)]
const NIUGIT_RECIPE_ID: &str = "niugit";
/// The primary install command the wizard's Install pick recommends
/// (shown in the menu and the summary row). Windows-only, same gate as
/// the question.
#[cfg(windows)]
const NIUGIT_WPM_COMMAND: &str = "wpm install niugit";

/// Tools probed on PATH during preflight; drives the environment summary.
/// These are application tools (plus git/docker/npm style runtimes) —
/// probes only: installing them is the system package manager's job (wpm
/// first on Windows, owner correction 2026-10-03; native managers
/// elsewhere). niu never downloads executables.
const PROBED_TOOLS: &[&str] = &[
    "git", "fzf", "eza", "bat", "starship", "zoxide", "fd", "rg", "dust", "duf", "erd", "direnv",
    "kubectl", "docker", "npm", "thefuck",
];

/// Windows-only probe additions: wpm exists only on Windows — the name must
/// never surface (probe lists included) on other platforms.
#[cfg(windows)]
const PLATFORM_PROBED_TOOLS: &[&str] = &["wpm"];
#[cfg(not(windows))]
const PLATFORM_PROBED_TOOLS: &[&str] = &[];

/// One release-bundle component (niubash#230 preinstall manifest,
/// `scripts/release/preinstall.json`): the release CI installs it into the
/// staged WinuxCmd root as `<root>\opt\<opt_dir>\<exe>` plus a winuxcmd.exe
/// hardlink shim in `<root>\usr\bin`. Detection treats an existing payload or
/// shim as "already present" so the wizard never reports a bundled component
/// as missing and never re-recommends installing it.
struct BundledComponent {
    /// The wpm package name (as in the manifest) — used for labeling.
    package: &'static str,
    /// Subdirectory under `<root>\opt` carrying the payload.
    opt_dir: &'static str,
    /// Executable file names inside the opt payload directory.
    exes: &'static [&'static str],
    /// Shim names (hardlinks) under `<root>\usr\bin`.
    shims: &'static [&'static str],
    /// `PROBED_TOOLS` names this component satisfies when present.
    tools: &'static [&'static str],
}

/// The components the release bundle ships (#230). Keep in sync with
/// `scripts/release/preinstall.json`; detection is fail-open — a component
/// the release leg skipped (arm64: niugit/gawk) simply stays undetected.
const BUNDLED_COMPONENTS: &[BundledComponent] = &[
    BundledComponent {
        package: "gawk",
        opt_dir: "gawk",
        exes: &["gawk.exe", "awk.exe"],
        shims: &["gawk.exe", "awk.exe"],
        tools: &["gawk", "awk"],
    },
    BundledComponent {
        package: "niugit",
        opt_dir: "niugit",
        exes: &["git.exe"],
        shims: &["git.exe"],
        tools: &["git"],
    },
    BundledComponent {
        package: "ripgrep",
        opt_dir: "ripgrep",
        exes: &["rg.exe"],
        shims: &["rg.exe"],
        tools: &["rg"],
    },
    BundledComponent {
        package: "fd",
        opt_dir: "fd",
        exes: &["fd.exe"],
        shims: &["fd.exe"],
        tools: &["fd"],
    },
];

/// Which bundled components are present under a WinuxCmd installation root:
/// a component counts as present when its opt payload *or* any usr\bin shim
/// exists (the shim alone already forwards into the payload through the
/// winuxcmd dispatcher). Pure over its inputs so tests can stage a fake
/// root; `None` root (no winuxcmd found) means nothing is bundled.
fn bundled_components_at(root: Option<&std::path::Path>) -> Vec<&'static str> {
    let Some(root) = root else {
        return Vec::new();
    };
    let usr_bin = root.join("usr").join("bin");
    BUNDLED_COMPONENTS
        .iter()
        .filter(|c| {
            let opt = root.join("opt").join(c.opt_dir);
            c.exes.iter().any(|exe| opt.join(exe).is_file())
                || c.shims.iter().any(|shim| usr_bin.join(shim).is_file())
        })
        .map(|c| c.package)
        .collect()
}

/// The bundled components of the *running* installation: the opt/ tree of
/// the discovered winuxcmd root. Shared with `niu doctor` so both surfaces
/// report the same truth.
pub(crate) fn bundled_components() -> Vec<&'static str> {
    bundled_components_at(
        crate::winuxcmd::find_winuxcmd()
            .as_deref()
            .map(crate::winuxcmd::installation_root)
            .as_deref(),
    )
}

// ── Wizard language ─────────────────────────────────────────────────────────
//
// The wizard is the one niubash surface every new user reads, so it carries a
// built-in Chinese string table alongside English — no external catalogs, no
// framework. Detection order: an explicit `NIU_LANG` override wins, then
// POSIX-style `LC_ALL`/`LANG`, then the Windows UI language. Any string
// without a translation falls back to English, and the generated rc file
// itself always stays English (it is bash code plus comments).

/// UI language for the setup wizard.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Lang {
    #[default]
    En,
    Zh,
}

impl Lang {
    fn detect() -> Lang {
        if let Some(value) = std::env::var_os("NIU_LANG") {
            let value = value.to_string_lossy().to_lowercase();
            if value.starts_with("zh") {
                return Lang::Zh;
            }
            if !value.is_empty() {
                return Lang::En;
            }
        }
        for name in ["LC_ALL", "LANG"] {
            if let Some(value) = std::env::var_os(name) {
                let value = value.to_string_lossy().to_lowercase();
                if value.starts_with("zh") {
                    return Lang::Zh;
                }
                if !value.is_empty() {
                    return Lang::En;
                }
            }
        }
        if ui_language_is_chinese() {
            return Lang::Zh;
        }
        Lang::En
    }

    /// Translate a wizard string (English literal as the key). Untranslated
    /// keys fall back to the English text, so partial translations stay safe.
    fn tr<'a>(&self, en: &'a str) -> &'a str {
        match self {
            Lang::En => en,
            Lang::Zh => zh(en).unwrap_or(en),
        }
    }
}

/// True when the wizard language resolves to Chinese. Shared with the
/// interactive menu hint so the one line it owns matches the wizard around
/// it; the i18n itself stays embedded in this module.
pub(crate) fn wizard_lang_is_chinese() -> bool {
    Lang::detect() == Lang::Zh
}

/// True when the Windows user UI language is Chinese (any sublanguage).
#[cfg(windows)]
fn ui_language_is_chinese() -> bool {
    // LANG_CHINESE is the primary-language id 0x04; the low 10 bits of a
    // LANGID carry the primary language.
    const LANG_CHINESE: u32 = 0x04;
    let langid = unsafe { windows_sys::Win32::Globalization::GetUserDefaultUILanguage() } as u32;
    (langid & 0x3FF) == LANG_CHINESE
}

#[cfg(not(windows))]
fn ui_language_is_chinese() -> bool {
    false
}

/// How the `{git}` prompt segment is produced — or whether starship owns the
/// whole prompt outright.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GitBackend {
    /// Host-provided git snapshot rendered by the niubash prompt.
    #[default]
    Native,
    /// `starship module git_branch/git_status` renders just the `{git}`
    /// segment; the niubash prompt template and theme still apply.
    StarshipSegment,
    /// The bundle `starship` plugin runs `starship init bash`, which owns
    /// PS1/PROMPT_COMMAND — niubash prompt template and internal git status
    /// are disabled.
    StarshipFull,
}

/// Everything the wizard can write into `~/.niubashrc`. The built-in
/// theme/plugin stack is retired (niubash#145), so this is down to the
/// external-source theme pick plus display preferences and aliases.
///
/// Empty-string fields mean "no override": the generated rc omits the line
/// entirely, so wizard "skip" paths stay side-effect free.
#[derive(Debug, Clone, Default)]
pub struct WizardConfig {
    /// External-source theme name (oh-my-bash); empty means no theme block.
    pub theme: String,
    /// Source id (e.g. "oh-my-bash") when `theme` is an external-source
    /// theme; the rc activates it through the guarded source loader and the
    /// bash-compatible PS1 channel.
    pub theme_source_id: Option<String>,
    pub cwd_style: String,
    pub completion_style: String,
    /// Extra `alias name='cmd'` lines appended to the generated rc.
    pub aliases: Vec<(String, String)>,
}

/// The theme question's outcome. `Keep` writes no theme lines, mirroring
/// whatever is already active (product defaults on a fresh install).
#[derive(Debug, Clone, PartialEq, Eq)]
enum ThemePick {
    Keep,
    External { name: String, source_id: String },
}

/// One theme in the wizard gallery.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ThemeGalleryEntry {
    pub name: String,
    pub source_id: String,
    pub adapter: String,
}

/// The theme gallery: themes from trusted external plugin-manager sources
/// (oh-my-bash), sorted by name. The built-in/user TOML theme layers are
/// retired with the plugin stack (niubash#145).
#[derive(Debug, Clone)]
struct ThemeGallery {
    entries: Vec<ThemeGalleryEntry>,
}

fn theme_gallery() -> ThemeGallery {
    let mut external: Vec<ThemeGalleryEntry> = crate::plugins::sources::source_theme_entries()
        .into_iter()
        .map(|entry| ThemeGalleryEntry {
            name: entry.name,
            source_id: entry.source_id,
            adapter: entry.adapter_display,
        })
        .collect();
    // Sort by (name, source priority, source id), then keep the FIRST entry
    // per name. The priority tier is the fix for the journey's J5 gap
    // (run-13, 2026-10-04): with several frameworks installed, both may ship
    // a same-named theme (powerline-multiline exists in oh-my-bash AND
    // bash-it) and the old plain-name sort kept whichever the registry
    // happened to list first — silently routing the pick through the other
    // framework's adapter. Resolution now follows the product's framework
    // order: oh-my-bash is niu's primary external framework (the base of
    // every built-in collection, the rc template's default theme channel),
    // so a shared name activates through ITS native loader; every other
    // source follows alphabetical tie-break so the gallery stays
    // deterministic regardless of registry order.
    external.sort_by(|a, b| {
        a.name
            .cmp(&b.name)
            .then_with(|| {
                crate::plugins::sources::primary_theme_source_rank(&a.source_id).cmp(
                    &crate::plugins::sources::primary_theme_source_rank(&b.source_id),
                )
            })
            .then_with(|| a.source_id.cmp(&b.source_id))
    });
    let mut seen = BTreeSet::new();
    external.retain(|entry| seen.insert(entry.name.to_ascii_lowercase()));
    ThemeGallery { entries: external }
}

/// The theme question itself: option 0 is Skip — described by whatever
/// `current` is — then one row per gallery entry with a live preview. Q1
/// and the post-install pick (owner ruling 2026-10-03) share this exact
/// presentation, so both land in the rc through the same ThemePick path.
/// `None` = Ctrl-C.
///
/// The live preview (niubash#170) renders the highlighted theme's real PS1
/// in the pane below the menu: a child `niu` sources the theme's managed
/// block in a sandboxed environment and the ready bytes are painted here —
/// the same [`crate::plugins::theme_preview::GalleryPreviews`] cache the
/// `niu plugin ui` theme section uses. The callback stays synchronous: the
/// Measure pass answers from constants (no renders), a Draw pass starts the
/// theme's render lazily and reads the cache (or the bounded placeholder).
fn ask_theme_question(
    io: &mut WizardIo,
    t: &Lang,
    gallery: &ThemeGallery,
    current: &ThemePick,
) -> Option<ThemePick> {
    use crate::interactive_menu::PreviewPhase;
    use crate::plugins::theme_preview::{GalleryPreviews, PreviewState, RENDERING_PLACEHOLDER};

    let mut options = vec![match current {
        ThemePick::Keep => t.tr("Skip - keep my current theme").to_string(),
        pick @ ThemePick::External { .. } => format!(
            "{} ({})",
            t.tr("Skip - keep my current theme"),
            describe_theme_pick(pick, *t)
        ),
    }];
    for entry in &gallery.entries {
        options.push(entry.name.clone());
    }
    let theme_refs: Vec<&str> = options.iter().map(String::as_str).collect();
    let previews = GalleryPreviews::new();
    let preview = move |phase: PreviewPhase, i: usize| -> Vec<String> {
        match phase {
            // Layout sweep: constants only — never start a render per option.
            PreviewPhase::Measure => GalleryPreviews::measure_placeholder(),
            PreviewPhase::Draw if i == 0 => match current {
                ThemePick::Keep => vec![t.tr("current look unchanged").to_string()],
                pick => vec![format!(
                    "{} {}",
                    t.tr("keep"),
                    describe_theme_pick(pick, *t)
                )],
            },
            PreviewPhase::Draw => {
                let entry = &gallery.entries[i - 1];
                // Queue the next neighbor so arrow-key browsing finds its
                // preview already rendered (never waits, niubash#170).
                if let Some(next) = gallery.entries.get(i) {
                    previews.prefetch(&next.source_id, &next.name);
                }
                let mut lines = vec![format!("{} · {}", entry.name, entry.adapter)];
                // Zero grace: the callback must be instant — a fast arrow
                // walk pays this per key. The menu's async pane pickup
                // repaints when the render lands.
                lines.extend(
                    match previews.state_for(
                        &entry.source_id,
                        &entry.name,
                        std::time::Duration::ZERO,
                    ) {
                        PreviewState::Ready(prompt_lines) => prompt_lines,
                        PreviewState::Rendering => vec![t.tr(RENDERING_PLACEHOLDER).to_string()],
                        PreviewState::Unavailable(reason) => {
                            vec![format!("{}{reason})", t.tr("(preview unavailable: "))]
                        }
                    }
                    .into_iter()
                    .take(crate::plugins::theme_preview::MAX_PREVIEW_LINES),
                );
                lines
            }
        }
    };
    let hint = t.tr("  │  external themes from your trusted plugin sources; Skip changes nothing");
    let idx = io.choice_preview(
        t.tr("  \u{1f3a8}  Pick a theme"),
        0,
        &theme_refs,
        hint,
        &preview,
    )?;
    if idx > 0 {
        let entry = &gallery.entries[idx - 1];
        return Some(ThemePick::External {
            name: entry.name.clone(),
            source_id: entry.source_id.clone(),
        });
    }
    Some(ThemePick::Keep)
}

/// A curated setup preset: an alias-comfort level for `niu setup --preset`.
/// The plugin/theme parts of presets retired with the built-in stack
/// (niubash#145); what remains is aliases plus display preferences.
#[derive(Debug, Clone)]
pub struct Preset {
    pub name: String,
    pub summary: String,
    pub cwd_style: String,
    pub completion_style: String,
    /// Aliases always written into the rc.
    pub aliases: BTreeMap<String, String>,
    /// Binary -> aliases written only when the binary is on PATH.
    pub conditional_aliases: BTreeMap<String, BTreeMap<String, String>>,
}

impl Preset {
    /// Look up a built-in preset by name; panics only on a programmer error.
    fn builtin(name: &str) -> Preset {
        builtin_presets()
            .into_iter()
            .find(|p| p.name == name)
            .expect("built-in preset exists")
    }

    /// Expand this preset into a `WizardConfig` for the probed environment.
    /// Human-readable notes about skipped aliases go to `notes`.
    fn to_config(&self, probe: &EnvProbe, notes: &mut Vec<String>, lang: Lang) -> WizardConfig {
        let mut aliases: Vec<(String, String)> = self
            .aliases
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        for (bin, map) in &self.conditional_aliases {
            if probe.on_path(bin) {
                aliases.extend(map.iter().map(|(k, v)| (k.clone(), v.clone())));
            } else {
                notes.push(fill(
                    lang.tr("aliases for '{}' skipped (not found on PATH)"),
                    &[bin],
                ));
            }
        }

        WizardConfig {
            cwd_style: self.cwd_style.clone(),
            completion_style: self.completion_style.clone(),
            aliases,
            ..WizardConfig::default()
        }
    }
}

/// The presets compiled into niubash, kept as alias-comfort levels.
fn builtin_presets() -> Vec<Preset> {
    let recommended_aliases: BTreeMap<String, String> = [
        ("ll", "ls -la"),
        ("la", "ls -a"),
        ("l", "ls -F"),
        ("..", "cd .."),
        ("...", "cd ../.."),
        ("cls", "clear"),
    ]
    .iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect();

    let mut recommended_cond_aliases: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
    let eza: BTreeMap<String, String> = [
        ("ls", "eza --icons --git --group-directories-first"),
        ("ll", "eza -lh --icons --git --group-directories-first"),
        ("la", "eza -la --icons --git --group-directories-first"),
        ("lt", "eza --tree --level=2 --icons"),
    ]
    .iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect();
    recommended_cond_aliases.insert("eza".to_string(), eza);
    for (bin, alias, cmd) in [
        ("bat", "cat", "bat -pp"),
        ("dust", "du", "dust"),
        ("duf", "df", "duf"),
        ("fd", "files", "fd"),
        ("erd", "tree", "erd --icons"),
    ] {
        recommended_cond_aliases.insert(
            bin.to_string(),
            [(alias.to_string(), cmd.to_string())].into_iter().collect(),
        );
    }
    // Command-layer entry alias (kept per the 2026-10-03 wpm retraction
    // ruling): `apt` opens the bundled Unix command layer for users who
    // actually have wpm — it is an entry point, never a tool-install
    // recommendation (application tools install through the plugin driver).
    // Conditional on the probe, and Windows-only at compile time: the `wpm`
    // string must never surface in non-Windows builds.
    #[cfg(windows)]
    recommended_cond_aliases.insert(
        "wpm".to_string(),
        [("apt".to_string(), "wpm".to_string())]
            .into_iter()
            .collect(),
    );

    vec![
        Preset {
            name: "recommended".into(),
            summary: "curated daily driver: smart aliases".into(),
            cwd_style: "home".into(),
            completion_style: "column".into(),
            aliases: recommended_aliases,
            conditional_aliases: recommended_cond_aliases,
        },
        Preset {
            name: "poweruser".into(),
            summary: "same aliases plus every tool-conditioned extra".into(),
            cwd_style: "home".into(),
            completion_style: "list".into(),
            aliases: BTreeMap::new(),
            conditional_aliases: BTreeMap::new(),
        },
        Preset {
            name: "minimal".into(),
            summary: "safe everywhere: no extra aliases".into(),
            cwd_style: "home".into(),
            completion_style: "column".into(),
            aliases: BTreeMap::new(),
            conditional_aliases: BTreeMap::new(),
        },
    ]
}

fn load_presets() -> Vec<Preset> {
    builtin_presets()
}

/// Environment facts collected once, before any question is asked.
struct EnvProbe {
    windows_terminal: bool,
    mintty_hint: bool,
    nerd_font: bool,
    command_links: bool,
    /// Names from `PROBED_TOOLS` that resolved on PATH.
    tools: BTreeSet<String>,
    /// Release-bundle components found under the winuxcmd opt/ tree (#230):
    /// `BUNDLED_COMPONENTS` package names. Their tool names count as present
    /// even when nothing resolves on PATH yet, so the wizard neither
    /// recommends re-installing them nor skips aliases conditioned on them.
    bundled: Vec<&'static str>,
}

impl EnvProbe {
    /// Probed for the running installation; testable form below.
    fn collect() -> Self {
        Self::from_parts(Self::probe_tools(), bundled_components())
    }

    /// The tools that resolve on PATH right now.
    fn probe_tools() -> BTreeSet<String> {
        let mut tools: BTreeSet<String> = PROBED_TOOLS
            .iter()
            .chain(PLATFORM_PROBED_TOOLS.iter())
            .filter(|tool| on_path(tool))
            .map(|tool| tool.to_string())
            .collect();
        // Windows-only: wpm is also usable through `winuxcmd.exe wpm`
        // without a command link. The name must never surface elsewhere.
        #[cfg(windows)]
        if wpm_available() {
            tools.insert("wpm".to_string());
        }
        tools
    }

    /// Test seam: build a probe from explicit facts instead of probing the
    /// live machine.
    fn from_parts(tools: BTreeSet<String>, bundled: Vec<&'static str>) -> Self {
        EnvProbe {
            windows_terminal: std::env::var_os("WT_SESSION").is_some(),
            mintty_hint: std::env::var_os("MSYSTEM").is_some()
                || std::env::var("TERM_PROGRAM")
                    .map(|v| v.eq_ignore_ascii_case("mintty"))
                    .unwrap_or(false),
            nerd_font: crate::fonts::nerd_font_installed(),
            command_links: crate::winuxcmd::command_links_ready(),
            tools,
            bundled,
        }
    }

    fn on_path(&self, tool: &str) -> bool {
        self.tools.contains(tool)
            || self.bundled.iter().any(|pkg| {
                BUNDLED_COMPONENTS
                    .iter()
                    .find(|c| c.package == *pkg)
                    .is_some_and(|c| c.tools.contains(&tool))
            })
    }

    fn print_summary(&self, lang: Lang) {
        let terminal = if self.windows_terminal {
            "Windows Terminal"
        } else {
            lang.tr("classic console")
        };
        // Source annotation: a tool satisfied by the release bundle is
        // labeled "bundled" so the summary never passes a shipped component
        // off as a system install (#230 follow-up). A name that also
        // resolves on PATH prints unannotated — PATH is what actually runs.
        let bundled_names = self.bundled_tool_names();
        let tools = if self.tools.is_empty() && bundled_names.is_empty() {
            lang.tr("none detected").to_string()
        } else {
            let mut names: Vec<String> = self
                .tools
                .union(&bundled_names)
                .map(|tool| {
                    if bundled_names.contains(tool) {
                        format!("{tool} (bundled)")
                    } else {
                        tool.clone()
                    }
                })
                .collect();
            names.sort();
            names.join(" ")
        };
        let bundle = if self.bundled.is_empty() {
            lang.tr("none").to_string()
        } else {
            format!(
                "{} ({})",
                self.bundled.join(" "),
                lang.tr("shipped with the release")
            )
        };
        let label = |en: &str| pad_display(lang.tr(en), 14);
        println!();
        println!("  \u{1f50d}  {}", lang.tr("Environment"));
        println!("  \u{2502}  {} {}", label("terminal"), terminal);
        println!(
            "  \u{2502}  {} {}",
            label("nerd font"),
            if self.nerd_font {
                lang.tr("detected")
            } else {
                lang.tr("not found")
            }
        );
        println!(
            "  \u{2502}  {} {}",
            label("command links"),
            if self.command_links {
                lang.tr("ready")
            } else {
                lang.tr("missing")
            }
        );
        println!("  \u{2502}  {} {}", label("tools"), tools);
        println!("  \u{2502}  {} {}", label("bundle"), bundle);
    }

    /// The `PROBED_TOOLS` names covered by the detected bundle, excluding the
    /// ones already resolved on PATH (those print unannotated as system).
    fn bundled_tool_names(&self) -> BTreeSet<String> {
        self.bundled
            .iter()
            .filter_map(|pkg| BUNDLED_COMPONENTS.iter().find(|c| c.package == *pkg))
            .flat_map(|c| c.tools.iter())
            .filter(|tool| !self.tools.contains(**tool))
            .map(|tool| tool.to_string())
            .collect()
    }
}

/// Interactive question driver with fast-forward and abort handling.
///
/// Esc on any question fast-forwards: every remaining question silently takes
/// its default and the flow lands on the summary. Ctrl-C aborts the wizard.
struct WizardIo {
    interactive: bool,
    fast_forward: bool,
    /// Tests only: queued answers consumed by [`WizardIo::choice_inner`]
    /// before any console interaction — the wizard's test IO seam, so unit
    /// tests drive question flows (including the post-install theme pick)
    /// without a terminal.
    #[cfg(test)]
    script: std::collections::VecDeque<usize>,
}

impl WizardIo {
    fn new(interactive: bool) -> Self {
        WizardIo {
            interactive,
            fast_forward: false,
            #[cfg(test)]
            script: std::collections::VecDeque::new(),
        }
    }

    /// Tests only: an "interactive" io whose answers come from `answers`
    /// (one per question, in order).
    #[cfg(test)]
    fn scripted(answers: &[usize]) -> Self {
        WizardIo {
            interactive: true,
            fast_forward: false,
            script: answers.iter().copied().collect(),
        }
    }

    fn choice(
        &mut self,
        label: &str,
        default_idx: usize,
        options: &[&str],
        help: &str,
    ) -> Option<usize> {
        self.choice_inner(label, default_idx, options, help, None)
    }

    fn choice_preview(
        &mut self,
        label: &str,
        default_idx: usize,
        options: &[&str],
        help: &str,
        preview: &dyn Fn(crate::interactive_menu::PreviewPhase, usize) -> Vec<String>,
    ) -> Option<usize> {
        self.choice_inner(label, default_idx, options, help, Some(preview))
    }

    fn choice_inner(
        &mut self,
        label: &str,
        default_idx: usize,
        options: &[&str],
        help: &str,
        preview: Option<&dyn Fn(crate::interactive_menu::PreviewPhase, usize) -> Vec<String>>,
    ) -> Option<usize> {
        // Test seam: a queued answer wins before any console interaction.
        // `usize::MAX` scripts a Ctrl-C (None).
        #[cfg(test)]
        if let Some(idx) = self.script.pop_front() {
            return if idx == usize::MAX {
                None
            } else {
                Some(idx.min(options.len().saturating_sub(1)))
            };
        }
        if !self.interactive || self.fast_forward || options.is_empty() {
            return Some(default_idx.min(options.len().saturating_sub(1)));
        }
        let selection = match preview {
            Some(pv) => {
                interactive_menu::interactive_choice_ex(label, options, default_idx, help, Some(pv))
            }
            None => interactive_menu::interactive_choice(label, options, default_idx, help),
        };
        match selection {
            Selection::Confirmed(idx) => Some(idx),
            Selection::UseDefault => {
                self.fast_forward = true;
                println!(
                    "  \x1b[2m{} → {}\x1b[0m",
                    label.trim(),
                    options[default_idx]
                );
                Some(default_idx.min(options.len().saturating_sub(1)))
            }
            Selection::Abort => None,
        }
    }

    /// The final Apply/Cancel gate. Unlike ordinary questions this never
    /// honours fast-forward: Esc fast-forward still stops here for an
    /// explicit answer, and Esc *on* this prompt means Cancel.
    fn confirm(&mut self, label: &str, options: &[&str]) -> Option<usize> {
        if !self.interactive || options.is_empty() {
            return Some(0);
        }
        match interactive_menu::interactive_choice(label, options, 0, "") {
            Selection::Confirmed(idx) => Some(idx),
            Selection::UseDefault => Some(options.len() - 1),
            Selection::Abort => None,
        }
    }
}

/// Localized Ctrl-C abort note for the wizard. A free function (not a
/// method) because the `ask!` macro's hygiene rules hide caller locals.
fn print_cancelled_note() {
    println!();
    println!(
        "  \u{1f6d1}  {}",
        Lang::detect().tr("Setup cancelled \u{2014} nothing was written.")
    );
}

/// `format!` for translated templates: substitutes `{}` placeholders left to
/// right. `format!` requires a literal format string, so dynamic translated
/// templates go through this helper instead.
fn fill(template: &str, args: &[&dyn std::fmt::Display]) -> String {
    let mut out = template.to_string();
    for arg in args {
        match out.find("{}") {
            Some(pos) => out.replace_range(pos..pos + 2, &arg.to_string()),
            None => break,
        }
    }
    out
}

/// Unwrap an `Option` answer; `None` means Ctrl-C — stop the wizard cleanly.
macro_rules! ask {
    ($call:expr) => {
        match $call {
            Some(value) => value,
            None => {
                print_cancelled_note();
                return Ok(());
            }
        }
    };
}

/// Returns `true` if the user has never run the setup wizard before
/// (i.e. no primary/compat rc file and no setup marker). Legacy/managed TOML
/// metadata no longer blocks first-run rc onboarding.
pub fn is_first_run() -> bool {
    let home = setup_home_dir();
    for name in [PRIMARY_RC_FILE, COMPAT_RC_FILE] {
        if home.join(name).is_file() {
            return false;
        }
    }
    !home.join(".niubash").join(SETUP_DONE_FILE).is_file()
}

/// Run the interactive setup wizard.
///
/// Prints a welcome banner, asks the user a few questions with defaults,
/// writes `~/.niubashrc`, and creates the `.setup-done` marker.
pub fn run_wizard() -> anyhow::Result<()> {
    run_wizard_inner(false)
}

/// Re-run the setup wizard even if the user already has a startup rc.
pub fn rerun_wizard() -> anyhow::Result<()> {
    run_wizard_inner(true)
}

/// Render the logo to lines and print it alongside welcome text.
fn display_welcome_side_by_side(reconfigure: bool, lang: Lang) {
    let width = crate::interactive_menu::term_width();
    let logo_cols = if width >= 100 { 48 } else { 32 };
    // niubash#195: when stdout is not a terminal (pipe, redirect, CI) the
    // ANSI pixel-art logo degrades to plain text — same content, no escapes.
    let plain = !crate::terminal::stdout_is_terminal();
    let logo_lines: Vec<String> = if plain {
        Vec::new()
    } else {
        crate::logo::render_logo_to_string(logo_cols)
            .lines()
            .map(String::from)
            .collect()
    };

    let mut content = Vec::new();
    content.push(String::new());
    content.push(format!(
        " {}  {} {}",
        "\u{1f389}",
        lang.tr("Welcome to Niubash"),
        format!("v{}!", env!("CARGO_PKG_VERSION"))
    ));
    content.push(format!(
        " {}  {}",
        "\u{2728}",
        lang.tr("A native Rust implementation of bash for Windows \u{2014} \
             no WSL, no MSYS2, no emulation layer.")
    ));
    content.push(String::new());
    if reconfigure {
        content.push(
            lang.tr("Reconfigure your interactive prompt/plugins. Existing rc will be backed up.")
                .to_string(),
        );
    } else {
        content.push(lang.tr("Let\u{2019}s get you set up.").to_string());
    }
    content.push(String::new());

    crate::interactive_menu::print_side_by_side(&logo_lines, &content, 100);
}

fn run_wizard_inner(reconfigure: bool) -> anyhow::Result<()> {
    let home = setup_home_dir();
    let lang = Lang::detect();
    let t = lang;

    display_welcome_side_by_side(reconfigure, lang);

    let probe = EnvProbe::collect();
    let mut io = WizardIo::new(crate::terminal::stdio_is_interactive());

    if io.interactive {
        probe.print_summary(lang);
    } else if probe.mintty_hint {
        println!();
        println!(
            "  \u{2139}\u{fe0f}  {}",
            t.tr("This terminal can't host interactive menus (Git Bash/MinTTY).")
        );
        println!(
            "  \u{2502}  {}",
            t.tr("Applying the 'minimal' preset. Re-run `niu setup` inside")
        );
        println!(
            "  \u{2502}  {}",
            t.tr("Windows Terminal, cmd, or PowerShell for the full wizard.")
        );
    }

    // Windows-only at compile time: the recovery hint names the wpm
    // command-layer verb, and `wpm` must never surface in non-Windows
    // builds (the command links themselves are a Windows-only concept).
    #[cfg(windows)]
    if io.interactive && !probe.command_links {
        println!();
        println!(
            "  \u{26a0}\u{fe0f}  {}",
            t.tr("WinuxCmd command links look missing (ls/cat/grep/ln).")
        );
        println!(
            "  \u{2502}  {}",
            t.tr("They are created automatically on startup; if Unix commands still")
        );
        println!(
            "  \u{2502}  {}",
            t.tr("fail after setup, restart niu or run `wpm links rebuild`.")
        );
    }

    // Non-interactive runs stay deterministic: apply the 'minimal' preset.
    if !io.interactive {
        let preset = Preset::builtin("minimal");
        let cfg = preset.to_config(&probe, &mut Vec::new(), lang);
        let backup_path = write_rc_and_mark_done(&home, &cfg, lang)?;
        let rc_created = backup_path.is_none();
        write_setup_journal(
            &home,
            &SetupJournal {
                rc_backup: backup_path,
                rc_created,
                preset: Some(preset.name.clone()),
                ..SetupJournal::default()
            },
        );
        return Ok(());
    }

    // --- Q1: theme gallery (external sources only) ---
    let gallery = theme_gallery();
    let current = current_theme_pick(&home);
    let mut theme_pick = ThemePick::Keep;
    if gallery.entries.is_empty() {
        println!();
        println!(
            "  {}",
            t.tr("No external themes installed yet - keeping the default look.")
        );
        println!(
            "  {}",
            t.tr("Browse the ecosystem any time with `niu plugin discover` (read-only).")
        );
    } else {
        theme_pick = ask!(ask_theme_question(&mut io, &t, &gallery, &current));
    }

    // --- Q2.5: plugin collection (only when the ecosystem is empty) ---
    // LazyVim-style progressive disclosure (study §10.2): first run offers
    // one bundled starting point; everything else stays behind `niu plugin
    // recipe list` / `niu plugin ui`. Applying only installs (untrusted);
    // trust and activation stay explicit user verbs.
    let collection_pick = ask_plugin_collection(&mut io, &t);

    // --- Q3: niu-git, offered once (never auto-installed, never nagged) ---
    // Windows-only question: niu-git is Windows-native git, and Linux/macOS
    // users already have native git. niu installs nothing itself (download
    // retraction 2026-10-04): the Install pick prints the recommendation —
    // wpm first (niu-git ships in the wpm index; owner correction
    // 2026-10-03), then the upstream releases page.
    let niu_git = match ask_niu_git(&mut io, &t, &home, &probe) {
        Some(choice) => choice,
        // niubash#179 L02-3: Ctrl-C here must print the same cancelled
        // note every other question prints — silent exit is a dead end.
        None => {
            print_cancelled_note();
            return Ok(());
        }
    };

    // --- Summary + explicit Apply gate ---
    let mut cfg = build_config(&theme_pick);
    print_config_summary(
        &cfg,
        &theme_pick,
        niu_git,
        collection_pick.as_ref().and_then(|pick| pick.as_deref()),
        lang,
    );
    let confirm_options = [t.tr("Apply"), t.tr("Cancel")];
    let confirm = ask!(io.confirm(
        t.tr("  \u{2705}  Apply this configuration?"),
        &confirm_options,
    ));
    if confirm == 1 {
        println!("  {}", t.tr("Nothing was written."));
        return Ok(());
    }

    let backup_path = write_rc_and_mark_done(&home, &cfg, lang)?;
    // The pick is recorded in the spec too (niubash#168): the rc block and
    // the spec entry must not disagree about theme ownership, or every
    // later sync re-materializes the old claim over this choice.
    record_theme_pick_in_spec(&theme_pick);

    // The niu-git question is Windows-only (niu-git is Windows-native git):
    // the Install branch — and the recommendation print — compiles only
    // there. Non-Windows wizard paths never reach NiuGitChoice::Install
    // (ask_niu_git returns Skip), so only the answer memory remains.
    #[cfg(windows)]
    if niu_git == NiuGitChoice::Install {
        recommend_niu_git(&home, lang);
    } else if niu_git == NiuGitChoice::NeverShow {
        write_niu_git_answer(&home, "never");
    }
    #[cfg(not(windows))]
    if niu_git == NiuGitChoice::NeverShow {
        write_niu_git_answer(&home, "never");
    }

    // The collection apply lands after the rc write: it installs sources
    // (untrusted) and prints executable-tool recommendations, collecting
    // per-entry failures the way lazy.nvim collects spec errors (study
    // §10.1) — a failed entry never fails the wizard run.
    let collection_journal = collection_pick
        .flatten()
        .and_then(|name| apply_plugin_collection(&name, lang));

    // --- Post-install theme pick (owner ruling 2026-10-03) ---
    // Sits between the collection apply and the journal write, so the run
    // still produces ONE journal + ONE finish screen whose undo lines
    // cover everything (collection sources + the theme pick through the
    // shared journal.theme entry). Asked only when the apply installed a
    // theme-bearing source; minimal/Skip collections ask nothing.
    let fresh_themes = post_install_theme_candidates(collection_journal.as_ref());
    if !fresh_themes.is_empty() {
        if let Some((name, source_id)) =
            run_post_install_theme_pick(&mut io, lang, &home, &fresh_themes, &mut cfg)
        {
            theme_pick = ThemePick::External { name, source_id };
        }
    }

    // --- Spec adoption: the run ends spec-managed (1.3.1) ---
    // The collection apply installs through the recipe drivers; in 1.3.0
    // that left the spec unwritten, so every later startup nagged
    // "installed but not declared" with no working migration verb.
    // Skipped when the apply landed nothing (total failure): adopting an
    // empty set would only materialize an empty spec file.
    if collection_journal
        .as_ref()
        .is_some_and(|journal| !journal.sources.is_empty())
    {
        adopt_installed_sources_into_spec(lang);
    }

    // Setup journal + per-entry undo (iron law 3: 失败可回滚). A theme
    // picked either at Q1 or post-install lands in the same field.
    let journal = SetupJournal {
        rc_backup: backup_path.clone(),
        // Fresh install (no previous rc): the receipt must cover the write
        // itself (niubash#179 L02-1).
        rc_created: backup_path.is_none(),
        theme: match &theme_pick {
            ThemePick::External { name, source_id } => Some((name.clone(), source_id.clone())),
            ThemePick::Keep => None,
        },
        preset: None,
        // Whatever lasting answer the run recorded ("never"/"recommended");
        // a plain Skip stays transient and journaled as none.
        niu_git: read_niu_git_answer(&home),
        collection: collection_journal,
    };
    write_setup_journal(&home, &journal);

    print_finish_screen(
        backup_path.as_deref(),
        &setup_undo_lines(&home, &journal),
        &setup_retry_lines(&journal),
        lang,
    );

    Ok(())
}

/// Apply the picked collection after the Apply gate (study §8/§10.2).
/// Failures are collected per entry (lazy.nvim Spec:log pattern): each one
/// is printed with its repair verb and the run continues — the function
/// itself never fails.
fn apply_plugin_collection(name: &str, lang: Lang) -> Option<CollectionJournal> {
    println!();
    match crate::plugins::distros::apply(name) {
        Ok(outcome) => {
            for report in &outcome.reports {
                println!("    - {}", report.summary);
            }
            for (recipe, error) in &outcome.failures {
                println!("    {} {}: {}", lang.tr("failed"), recipe, error);
            }
            if outcome.failures.is_empty() {
                println!(
                    "  {}  {}",
                    lang.tr("plugin collection"),
                    lang.tr("installed — review with `niu plugin trust <id>`")
                );
            } else {
                println!(
                    "  {}  {} {} {}",
                    lang.tr("plugin collection"),
                    outcome.failures.len(),
                    lang.tr("entries failed — retry with"),
                    format!("`niu plugin distro apply {name}`")
                );
            }
            // The journal records what ACTUALLY landed plus every failed
            // entry — never a bare success line over a partial failure.
            Some(CollectionJournal {
                name: outcome.name,
                sources: outcome.installed_sources,
                failed: outcome
                    .failures
                    .iter()
                    .map(|(recipe, _)| recipe.clone())
                    .collect(),
            })
        }
        Err(err) => {
            println!(
                "  \u{26a0}\u{fe0f}  {} '{name}': {err:#}",
                lang.tr("plugin collection")
            );
            // A totally failed apply is journaled too (zero sources, the
            // error recorded) — the run's history must not silently forget
            // that a collection was picked and failed.
            Some(CollectionJournal {
                name: name.to_string(),
                sources: Vec::new(),
                failed: vec![format!("apply failed: {err:#}")],
            })
        }
    }
}

/// The end-of-run spec adoption (1.3.1): after a collection apply — and
/// the post-install theme pick, so the chosen theme is part of the
/// snapshot — declare every installed source into
/// `~/.niubash/plugins.toml` by snapshotting the live selection
/// (`plugins::sync::adopt_installed_sources`, a defensive merge that never
/// clobbers pre-existing entries), then reconcile once so the run ends in
/// the exact state a plain `niu plugin sync` keeps. 1.3.0 left the spec
/// unwritten here and every later startup nagged "installed but not
/// declared" with no working migration verb; the wizard now ends
/// spec-managed (the spec is the single source of truth).
fn adopt_installed_sources_into_spec(lang: Lang) {
    match crate::plugins::sync::adopt_installed_sources() {
        Ok(adopted) if !adopted.is_empty() => {
            // Rows are informational here — the install reports were
            // printed above; this pass settles the registry's
            // spec_enabled/spec_theme so the next sync is a no-op.
            let _ = crate::plugins::sync::sync_spec(crate::plugins::sync::SyncOptions::default());
            println!();
            println!(
                "  \u{1f4dd}  {}",
                fill(
                    lang.tr("plugin spec written — {} source(s) declared; \
                         `niu plugin sync` keeps them in sync",),
                    &[&adopted.len().to_string()]
                )
            );
        }
        Ok(_) => {}
        Err(err) => println!(
            "  \u{26a0}\u{fe0f}  {}: {err:#}",
            lang.tr("could not write the plugin spec")
        ),
    }
}

// ── Post-install theme pick (owner ruling 2026-10-03: 装完即选主题) ──────────
//
// After the wizard applies a collection that installs theme-bearing
// sources, the SAME run offers the theme gallery built from the fresh
// source — one run, out-of-the-box. Trust stays explicit: the gallery
// lists trusted sources only, so the flow first asks the trust question
// (the wizard question IS the trust verb, same checksum tier as
// `niu plugin trust <id>`); declined → the exact one-liner to run later,
// nothing changes. The pick then goes through the same ThemePick →
// build_config → rc path as a Q1 pick, so the journal/undo contract
// covers it identically.

/// The freshly installed sources that can bear themes: untrusted, healthy,
/// and their adapter lists at least one theme asset (`asset_kinds` carries
/// `SourceAssetKind::as_str()` values — "theme"). Only these are named by
/// the trust question; bash-completion and friends stay out of it.
fn theme_bearing_untrusted_sources(installed: &[String]) -> Vec<String> {
    crate::plugins::sources::list_sources()
        .into_iter()
        .filter(|status| installed.contains(&status.record.id))
        .filter(|status| !status.degraded && !status.record.trusted)
        .filter(|status| status.asset_kinds.iter().any(|kind| kind == "theme"))
        .map(|status| status.record.id)
        .collect()
}

/// Whether the post-install theme question applies to a collection apply
/// (`None` = no collection, Skip, or an apply that failed — ask nothing).
fn post_install_theme_candidates(collection: Option<&CollectionJournal>) -> Vec<String> {
    let Some(collection) = collection else {
        return Vec::new();
    };
    theme_bearing_untrusted_sources(&collection.sources)
}

/// Run the post-install flow: the trust question, then (when trusted) the
/// theme gallery built from the freshly installed source. On a pick the
/// rc is rewritten through the same [`write_rc_and_mark_done`] path as a
/// Q1 pick (the intermediate backup of the theme-less rc is additive
/// history; the journal keeps pointing at the pre-run backup) and the
/// caller folds the returned `(name, source_id)` into the journal.
///
/// Returns `None` when nothing changed (declined, skipped, trust failed,
/// no gallery). Ctrl-C after Apply does NOT cancel the wizard: state was
/// already written, so the run still lands on the finish screen with its
/// undo receipts — the flow prints a skip note instead.
fn run_post_install_theme_pick(
    io: &mut WizardIo,
    lang: Lang,
    home: &std::path::Path,
    fresh_ids: &[String],
    cfg: &mut WizardConfig,
) -> Option<(String, String)> {
    let t = lang;
    let options = [
        format!(
            "{}  {}",
            pad_display(t.tr("Skip"), 14),
            t.tr("default — stay untrusted; nothing changes")
        ),
        format!(
            "{}  {}",
            pad_display(t.tr("Trust now"), 14),
            fill(
                t.tr("verify the checksum, then list themes from {}"),
                &[&fresh_ids.join(", ")]
            )
        ),
    ];
    let option_refs: Vec<&str> = options.iter().map(String::as_str).collect();
    let label = if fresh_ids.len() == 1 {
        fill(
            t.tr("  \u{1f510}  Trust '{}' now to list its themes?"),
            &[&fresh_ids[0]],
        )
    } else {
        t.tr("  \u{1f510}  Trust the freshly installed sources now to list their themes?")
            .to_string()
    };
    let idx = match io.choice(
        &label,
        0,
        &option_refs,
        t.tr("  |  this question is the `niu plugin trust` verb; Skip leaves the source untrusted"),
    ) {
        Some(idx) => idx,
        None => {
            println!();
            println!(
                "  \u{23ed}\u{fe0f}  {}",
                t.tr("Theme pick skipped — this run's summary follows")
            );
            return None;
        }
    };

    if idx == 0 {
        print_post_install_trust_hint(&t, fresh_ids);
        return None;
    }

    // Trust: `trust_source` re-verifies the tree checksum itself and
    // refuses on mismatch/degraded (the same gate `niu plugin trust`
    // runs), so a failure here names its repair verb and nothing flips.
    let mut trusted: Vec<String> = Vec::new();
    for id in fresh_ids {
        match crate::plugins::sources::trust_source(id) {
            Ok(record) => {
                println!(
                    "  \u{2705}  {}",
                    fill(
                        t.tr("trusted '{}' — its themes join the catalog"),
                        &[&record.id]
                    )
                );
                trusted.push(record.id);
            }
            Err(err) => println!(
                "  \u{26a0}\u{fe0f}  {}",
                fill(t.tr("could not trust '{}': {}"), &[id, &err])
            ),
        }
    }
    if trusted.is_empty() {
        print_post_install_trust_hint(&t, fresh_ids);
        return None;
    }

    let gallery = theme_gallery();
    if gallery.entries.is_empty() {
        println!();
        println!(
            "  {}",
            t.tr("no themes appeared after trust — pick later with `niu plugin enable <theme>`")
        );
        return None;
    }

    // The rc the wizard wrote at Apply time has no theme block, so the
    // "current" the Skip option describes is the default look.
    match ask_theme_question(io, &t, &gallery, &ThemePick::Keep) {
        Some(ThemePick::External { name, source_id }) => {
            cfg.theme = name.clone();
            cfg.theme_source_id = Some(source_id.clone());
            // Same write path as a Q1 pick: the guarded activation block
            // lands through generate_rc/write_rc_and_mark_done, so undo
            // (`niu plugin disable <theme>`) and rollback cover it.
            let _ = write_rc_and_mark_done(home, cfg, lang);
            // And the spec claim moves with it (niubash#168) — same reason
            // as the Q1 path: rc and spec must not disagree about who owns
            // the theme.
            record_theme_pick_in_spec(&ThemePick::External {
                name: name.clone(),
                source_id: source_id.clone(),
            });
            Some((name, source_id))
        }
        Some(ThemePick::Keep) => None,
        None => {
            println!();
            println!(
                "  \u{23ed}\u{fe0f}  {}",
                t.tr("Theme pick skipped — this run's summary follows")
            );
            None
        }
    }
}

/// The exact one-liner to run later when the trust question is declined
/// (or fails): trust the source, then re-run the wizard or enable a theme.
fn print_post_install_trust_hint(t: &Lang, ids: &[String]) {
    println!();
    println!(
        "  \u{21a9}\u{fe0f}  {}",
        t.tr("Theme pick skipped — trust later, then pick:")
    );
    for id in ids {
        println!("  \u{2502}    niu plugin trust {id}");
    }
    println!(
        "  \u{2502}    {}",
        t.tr("then re-run `niu setup` (or `niu plugin enable <theme>`)")
    );
}

/// Short human description of a theme pick ("classic", "robbyrussell ·
/// oh-my-bash").
fn describe_theme_pick(pick: &ThemePick, t: Lang) -> String {
    match pick {
        ThemePick::Keep => t.tr("default").to_string(),
        ThemePick::External { name, source_id } => format!("{name} · {source_id}"),
    }
}

/// The theme that is active right now: the assignment in the existing rc
/// (`OSH_THEME` marks an oh-my-bash pick, `BASH_IT_THEME` a bash-it pick —
/// the LAST theme-bearing block wins, mirroring rc load order). Used so
/// "Skip — keep my current theme" is honest and side-effect free.
fn current_theme_pick(home: &std::path::Path) -> ThemePick {
    let text = std::fs::read_to_string(home.join(PRIMARY_RC_FILE))
        .ok()
        .or_else(|| std::fs::read_to_string(home.join(COMPAT_RC_FILE)).ok());
    let Some(text) = text else {
        return ThemePick::Keep;
    };
    // The native NIU_THEME channel is retired (niubash#145); only the
    // external theme variables identify an active pick — each through its
    // own framework's managed block.
    let mut active: Option<(String, String)> = None;
    for raw in text.lines() {
        let line = raw.trim().strip_prefix("export ").unwrap_or(raw).trim();
        for (var, source_id) in [("OSH_THEME", "oh-my-bash"), ("BASH_IT_THEME", "bash-it")] {
            if let Some(rest) = line.strip_prefix(var).and_then(|r| r.strip_prefix('=')) {
                let value = rest.trim().trim_matches('\'').trim_matches('"');
                if !value.is_empty() {
                    active = Some((value.to_string(), source_id.to_string()));
                }
            }
        }
    }
    match active {
        Some((name, source_id)) => ThemePick::External { name, source_id },
        None => ThemePick::Keep,
    }
}

/// Build the rc configuration from the wizard answers. Skip paths mirror the
/// previous rc verbatim (theme lines omitted, plugin selection re-emitted),
/// and opting into completions only ever *adds* packs.
/// Build the rc configuration from the wizard answers. Skip keeps every
/// field at its default so the generated rc changes nothing.
fn build_config(theme_pick: &ThemePick) -> WizardConfig {
    let mut cfg = WizardConfig::default();
    if let ThemePick::External { name, source_id } = theme_pick {
        cfg.theme = name.clone();
        cfg.theme_source_id = Some(source_id.clone());
    }
    cfg
}

/// The user's answer to the niu-git question.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NiuGitChoice {
    Skip,
    #[cfg(windows)]
    Install,
    NeverShow,
}

// --- niu-git ask: Windows-only at compile time (niu-git is Windows-native
// git; on other platforms native git already exists). niu installs nothing
// itself (download retraction 2026-10-04): the Install pick prints the
// package-manager recommendation — wpm first on Windows (owner correction
// 2026-10-03: wpm is the first-class command-layer tool installer; niu-git
// ships in its index), plus the upstream releases page. Red lines
// unchanged: offered at most once per lasting answer, never
// auto-installed, cfg(windows) only.
#[cfg(windows)]
fn ask_niu_git(
    io: &mut WizardIo,
    t: &Lang,
    home: &std::path::Path,
    probe: &EnvProbe,
) -> Option<NiuGitChoice> {
    let mut niu_git = NiuGitChoice::Skip;
    if read_niu_git_answer(home).is_none() {
        // niubash#230 follow-up: the release bundle may already carry
        // niu-git (opt\niugit). Never re-offer installing what is already
        // there — record the fact so the question stays silent on re-runs.
        if probe.bundled.iter().any(|pkg| *pkg == "niugit") {
            write_niu_git_answer(home, "installed");
            return Some(NiuGitChoice::Skip);
        }
        let options = [
            format!(
                "{}  {}",
                pad_display(t.tr("Skip"), 14),
                t.tr("default — nothing is installed")
            ),
            format!(
                "{}  {}",
                pad_display(t.tr("Install"), 14),
                NIUGIT_WPM_COMMAND
            ),
            format!(
                "{}  {}",
                pad_display(t.tr("Don't ask again"), 14),
                t.tr("remember this and stop offering")
            ),
        ];
        let option_refs: Vec<&str> = options.iter().map(String::as_str).collect();
        let note = if probe.on_path("git") {
            t.tr("  │  a separate GPLv2 project; your current git keeps working either way")
        } else {
            t.tr("  │  a separate GPLv2 project — native Windows git without MSYS")
        };
        let idx = match io.choice(
            t.tr("  🧩  niu-git — Windows-native git experience?"),
            0,
            &option_refs,
            note,
        ) {
            Some(value) => value,
            None => return None, // cancelled
        };
        niu_git = match idx {
            1 => NiuGitChoice::Install,
            2 => NiuGitChoice::NeverShow,
            _ => NiuGitChoice::Skip,
        };
    }
    Some(niu_git)
}

#[cfg(not(windows))]
fn ask_niu_git(
    _io: &mut WizardIo,
    _t: &Lang,
    _home: &std::path::Path,
    _probe: &EnvProbe,
) -> Option<NiuGitChoice> {
    Some(NiuGitChoice::Skip)
}

fn wizard_answers_path(home: &std::path::Path) -> PathBuf {
    home.join(".niubash").join("wizard-answers.toml")
}

/// Q2.5 — one bundled starting point for a fresh install (study §10.2:
/// progressive disclosure). Offered only when no plugin sources are
/// installed yet; Skip is the default and the ecosystem stays one command
/// away. `None` = cancelled the wizard; `Some(None)` = skip; `Some(name)`
/// = picked a collection to apply after the Apply gate.
fn ask_plugin_collection(io: &mut WizardIo, t: &Lang) -> Option<Option<String>> {
    if !crate::plugins::sources::read_source_registry().is_empty() {
        return Some(None);
    }
    let options = [
        format!(
            "{}  {}",
            pad_display(t.tr("Skip"), 14),
            t.tr("default — browse later with `niu plugin recipe list`")
        ),
        format!(
            "{}  {}",
            pad_display("minimal", 14),
            t.tr("bash-completion only, no frameworks")
        ),
        format!(
            "{}  {}",
            pad_display("recommended", 14),
            t.tr("oh-my-bash + its default theme + completions")
        ),
        format!(
            "{}  {}",
            pad_display("full", 14),
            t.tr(
                "both frameworks + hooks + curated bash plugins; fzf/starship \
                 are suggested installs (niu downloads nothing)"
            )
        ),
    ];
    let option_refs: Vec<&str> = options.iter().map(String::as_str).collect();
    let idx = io.choice(
        t.tr("  \u{1f9f0}  Plugin collection?"),
        0,
        &option_refs,
        t.tr("  |  installs stay untrusted until `niu plugin trust`; Skip changes nothing"),
    )?; // cancelled
    let name = match idx {
        1 => Some("minimal"),
        2 => Some("recommended"),
        3 => Some("full"),
        _ => None,
    };
    Some(name.map(str::to_string))
}

/// The recorded niu-git answer, when the user gave a lasting one ("never",
/// "recommended" after a pick, or "installed" when the release bundle
/// already ships niu-git). While `None` the wizard may
/// offer the choice again on the next explicit `niu setup` run — a wizard
/// re-run is user-initiated, never a nag.
fn read_niu_git_answer(home: &std::path::Path) -> Option<String> {
    let text = std::fs::read_to_string(wizard_answers_path(home)).ok()?;
    for raw in text.lines() {
        let line = raw.trim();
        let Some(rest) = line.strip_prefix("niu_git") else {
            continue;
        };
        let value = rest.trim().strip_prefix('=')?.trim();
        let value = value.trim_matches('"').trim_matches('\'');
        if !value.is_empty() {
            return Some(value.to_string());
        }
    }
    None
}

fn write_niu_git_answer(home: &std::path::Path, value: &str) {
    let path = wizard_answers_path(home);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let body = format!(
        "# Answers the user gave explicitly during `niu setup`.\n\
         schema = \"{}\"\n\
         niu_git = \"{}\"\n",
        WIZARD_ANSWERS_SCHEMA, value
    );
    if let Err(err) = std::fs::write(&path, body) {
        println!(
            "  \u{26a0}\u{fe0f}  {} {err}",
            Lang::detect().tr("could not write")
        );
    }
}

/// Print the niu-git install recommendation the user explicitly picked
/// (download retraction, owner ruling 2026-10-04): niu itself installs
/// nothing — the pick prints the recipe's package-manager commands (wpm
/// first: niu-git ships in the wpm index; plus the upstream releases
/// page) and records the answer so the question does not nag again.
#[cfg(windows)]
fn recommend_niu_git(home: &std::path::Path, lang: Lang) {
    println!("  \u{1f4e6}  {NIUGIT_WPM_COMMAND}");
    match crate::plugins::recipes::install(NIUGIT_RECIPE_ID) {
        Ok(report) => {
            println!("    - {}", report.summary);
            for step in &report.next {
                println!("    {step}");
            }
        }
        Err(err) => println!("    {err:#}"),
    }
    println!(
        "  {}",
        lang.tr("niu installs nothing itself — run one of the commands above, then restart niu")
    );
    write_niu_git_answer(home, "recommended");
}

/// The final "how to change things later" block — one compact screen, in the
/// spirit of oh-my-zsh's post-install hints. Nothing here installs anything.
/// `failed` carries the retry lines for collection entries that did not
/// install, so a red apply cannot hide behind a green-looking finish
/// (journey run-13 observation: 1 entry failed, the finish screen silent).
fn print_finish_screen(
    backup_path: Option<&std::path::Path>,
    undo: &[String],
    failed: &[String],
    lang: Lang,
) {
    let t = lang;
    println!();
    if let Some(path) = backup_path {
        println!(
            "  \u{1f4e6}  {}",
            fill(t.tr("Previous rc backed up to {}"), &[&path.display()])
        );
    }
    // niubash#180 bottom line (mandatory minimum): the finish screen must
    // state the truth about the current session. The wizard process cannot
    // know what its parent is, so the line names both ways the new config
    // reaches a live prompt: a running niu applies the handoff marker at
    // its next prompt; anything else (PowerShell, cmd, an older niu) gets
    // it in new terminals or through an explicit `source ~/.niubashrc`.
    println!();
    println!(
        "  \u{26a1}  {}",
        t.tr("New config takes effect in new terminals.")
    );
    println!(
        "  \u{2502}    {}",
        t.tr("A running niu session applies it at the next prompt; other shells: `source ~/.niubashrc`")
    );
    if !failed.is_empty() {
        println!();
        println!(
            "  \u{26a0}\u{fe0f}  {}",
            t.tr("Collection entries failed — retry with:")
        );
        for line in failed {
            println!("  \u{2502}    {line}");
        }
    }
    if !undo.is_empty() {
        println!();
        println!("  \u{21a9}\u{fe0f}  {}", t.tr("Undo this run:"));
        for line in undo {
            println!("  \u{2502}    {line}");
        }
    }
    println!();
    println!("  \u{1f504}  {}", t.tr("Change things later:"));
    println!(
        "  \u{2502}    {}",
        t.tr("theme      `niu plugin list`  →  `niu plugin enable <name>`")
    );
    println!(
        "  \u{2502}    {}",
        t.tr("plugins    `niu plugin list`  ·  `niu plugin enable <name>`")
    );
    println!(
        "  \u{2502}    {}",
        t.tr("ecosystem  `niu plugin discover`  (sources & themes, read-only)")
    );
    println!(
        "  \u{2502}    {}",
        t.tr("font / WT  `niu font`  ·  `niu --install-wt-profile`")
    );
    println!("  \u{2502}    {}", t.tr("this guide `niu setup`"));
    println!();
}

/// Apply a named preset non-interactively (`niu setup --preset <name>`).
pub fn apply_preset(name: &str) -> anyhow::Result<()> {
    let lang = Lang::detect();
    let t = lang;
    let presets = load_presets();
    let preset = presets.iter().find(|p| p.name == name).ok_or_else(|| {
        let names = presets
            .iter()
            .map(|p| p.name.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        anyhow::anyhow!(
            "{} '{name}' \u{2014} {} {names}",
            t.tr("unknown preset"),
            t.tr("available:")
        )
    })?;
    let home = setup_home_dir();
    let probe = EnvProbe::collect();
    let mut notes = Vec::new();
    let cfg = preset.to_config(&probe, &mut notes, lang);
    for note in &notes {
        println!("  \u{2502}  {}", note);
    }
    let backup_path = write_rc_and_mark_done(&home, &cfg, lang)?;
    let journal = SetupJournal {
        rc_backup: backup_path.clone(),
        rc_created: backup_path.is_none(),
        preset: Some(preset.name.clone()),
        ..SetupJournal::default()
    };
    write_setup_journal(&home, &journal);
    println!(
        "  \u{2705}  {}",
        format!("{} '{}'.", t.tr("Preset applied:"), preset.name)
    );
    if let Some(path) = backup_path {
        println!(
            "  \u{1f4e6}  {}",
            fill(t.tr("Previous rc backed up to {}"), &[&path.display()])
        );
    }
    Ok(())
}

/// Render the final confirmation table before anything is written. Only the
/// explicitly picked rows appear; everything else is called out as untouched.
fn print_config_summary(
    cfg: &WizardConfig,
    theme_pick: &ThemePick,
    niu_git: NiuGitChoice,
    collection_pick: Option<&str>,
    lang: Lang,
) {
    let t = lang;
    let row = |en: &str, value: String| {
        println!("  \u{2502}  {} {}", pad_display(t.tr(en), 13), value);
    };
    println!();
    println!("  \u{1f4cb}  {}", t.tr("Summary"));
    row(
        "theme",
        match theme_pick {
            ThemePick::Keep => t.tr("unchanged").to_string(),
            ThemePick::External { name, .. } => format!("{} ({})", name, t.tr("oh-my-bash source")),
        },
    );
    row(
        "plugins",
        match collection_pick {
            Some(name) => format!(
                "{} ({})",
                name,
                t.tr("installs stay untrusted until reviewed")
            ),
            None => t.tr("unchanged").to_string(),
        },
    );
    row(
        "aliases",
        if cfg.aliases.is_empty() {
            t.tr("unchanged").to_string()
        } else {
            format!("{} {}", t.tr("add"), cfg.aliases.len())
        },
    );
    row(
        "niu-git",
        match niu_git {
            // The recommendation text is Windows-only (the question exists
            // only there); on other platforms the summary shows the same
            // "skipped" the wizard actually did (ask_niu_git always returns
            // Skip there).
            #[cfg(windows)]
            NiuGitChoice::Install => format!("{NIUGIT_WPM_COMMAND} ({})", t.tr("recommended")),
            NiuGitChoice::NeverShow => t.tr("don't ask again").to_string(),
            NiuGitChoice::Skip => t.tr("skipped").to_string(),
        },
    );
    println!("  \u{2502}  {}", t.tr("everything else stays untouched"));
    println!();
}

/// Record an external theme pick in the plugin spec (niubash#168): the rc
/// block the wizard just wrote and the spec entry must carry the same
/// ownership, or every later `niu plugin sync` re-materializes the stale
/// claim and reverts the pick. Failures are printed, never fatal — the rc
/// pick already landed and the reconciliation pre-pass can still heal a
/// missed claim on the next sync.
fn record_theme_pick_in_spec(pick: &ThemePick) {
    let ThemePick::External { name, source_id } = pick else {
        return;
    };
    if let Err(err) = crate::plugins::assets::claim_theme(source_id, name) {
        println!("  \u{26a0}\u{fe0f}  could not record the theme pick in the plugin spec: {err:#}");
    }
}

/// Write `~/.niubashrc` from `cfg` (backing up any existing file) and create
/// the `.setup-done` marker. Returns the backup path when one was made.
fn write_rc_and_mark_done(
    home: &std::path::Path,
    cfg: &WizardConfig,
    lang: Lang,
) -> anyhow::Result<Option<PathBuf>> {
    let rc_content = generate_rc(cfg);
    let rc_path = home.join(PRIMARY_RC_FILE);
    let backup_path = write_primary_rc(home, &rc_content)?;

    let niubash_dir = home.join(".niubash");
    let _ = std::fs::create_dir_all(&niubash_dir);
    let _ = std::fs::write(niubash_dir.join(SETUP_DONE_FILE), b"");
    // niubash#180: hand the new configuration to any live session. The rc
    // write already succeeded; a failed marker only costs the in-session
    // apply — the finish-screen truth line still tells the user how to
    // apply it by hand, so this stays non-fatal.
    let _ = std::fs::write(niubash_dir.join(APPLY_PENDING_FILE), b"");

    println!();
    println!(
        "  \u{2705}  {}",
        fill(lang.tr("Shell rc written to {}"), &[&rc_path.display()])
    );
    Ok(backup_path)
}

/// Path of the niubash#180 handoff marker.
fn apply_pending_path(home: &std::path::Path) -> PathBuf {
    home.join(".niubash").join(APPLY_PENDING_FILE)
}

/// Session side of the handoff (called from `shell.rs` at every prompt
/// draw): `true` exactly once per setup run — the marker is consumed before
/// the caller re-sources, so a failed apply can never loop the REPL.
pub fn take_apply_pending_marker(home: &std::path::Path) -> bool {
    let path = apply_pending_path(home);
    if !path.is_file() {
        return false;
    }
    matches!(std::fs::remove_file(&path), Ok(()))
}

/// Startup baseline (called when a session sources its rc): a marker left
/// by a setup that ran before this session started is stale by definition —
/// the rc being sourced right now already carries that configuration.
pub fn clear_stale_apply_pending_marker(home: &std::path::Path) {
    let _ = std::fs::remove_file(apply_pending_path(home));
}

/// The line the live session prints above its next prompt after it applied
/// a finished setup run's configuration (niubash#180).
pub(crate) fn session_apply_notice() -> &'static str {
    Lang::detect().tr("\u{21bb}  Applied the new configuration from ~/.niubashrc (`niu setup`).")
}

/// The honest failure line for the session side: the marker was consumed
/// but the re-source did not come back clean, so the session may still run
/// the old configuration and the user applies it by hand.
pub(crate) fn session_apply_failed_notice() -> &'static str {
    Lang::detect().tr(
        "\u{26a0}\u{fe0f}  Could not apply the new configuration \u{2014} run \
         `source ~/.niubashrc` to see the error.",
    )
}

// ── Helpers ──────────────────────────────────────────────────────────────────

/// `wpm` when the command link is on PATH, else `winuxcmd.exe wpm`. The
/// command-layer probe (environment summary / `apt` alias condition); the
/// wizard never installs anything through it — niu installs nothing at all
/// (download retraction 2026-10-04); wpm is only ever *recommended* as the
/// first-class Windows tool channel.
#[cfg(windows)]
fn wpm_command() -> Option<Command> {
    if on_path("wpm") {
        return Some(Command::new("wpm"));
    }
    crate::winuxcmd::find_winuxcmd().map(|exe| {
        let mut cmd = Command::new(exe);
        cmd.arg("wpm");
        cmd
    })
}

#[cfg(windows)]
fn wpm_available() -> bool {
    wpm_command().is_some()
}

// ── Presets ──────────────────────────────────────────────────────────────────

// ── Environment probing ─────────────────────────────────────────────────────

/// True when `tool` resolves to a file on PATH (`.exe`/`.bat`/`.cmd`/`.com`
/// or an extension-less name).
fn on_path(tool: &str) -> bool {
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    const EXTS: &[&str] = &["", ".exe", ".bat", ".cmd", ".com"];
    std::env::split_paths(&path).any(|dir| {
        EXTS.iter()
            .any(|ext| dir.join(format!("{tool}{ext}")).is_file())
    })
}

fn setup_home_dir() -> PathBuf {
    shell_home_dir().unwrap_or_else(|| PathBuf::from("."))
}

/// Guarded activation block for an external oh-my-bash theme (§3.3/§11.4):
/// set `OSH_THEME` first, then the adapter's canonical loader snippet inside
/// a managed block (`niu plugin enable/disable` edits the same markers).
/// The existence guard keeps the fallback layer alive when the source tree
/// is missing.
fn external_theme_activation(cfg: &WizardConfig, theme: &str) -> Option<String> {
    let source_id = cfg.theme_source_id.as_deref()?;
    if theme.is_empty() {
        return None;
    }
    crate::plugins::assets::build_theme_block(source_id, theme)
}

fn generate_rc(cfg: &WizardConfig) -> String {
    // Clean rc (niubash#145): no built-in plugin/theme stack lines, no
    // oh-my-niu bundle discovery, no NIU_PLUGINS/NIU_THEME assignments.
    // An external-source theme (oh-my-bash) activates through its guarded
    // loader and renders via the bash-compatible PS1 channel.
    let external = external_theme_activation(cfg, &cfg.theme);
    let mut header = String::new();
    if !cfg.cwd_style.is_empty() {
        header.push_str(&format!(
            "NIU_PROMPT_CWD_STYLE={}\n",
            shell_quote(cfg.cwd_style.as_str())
        ));
    }
    if !cfg.completion_style.is_empty() {
        header.push_str(&format!(
            "NIU_COMPLETION_STYLE={}\n",
            shell_quote(cfg.completion_style.as_str())
        ));
    }
    let alias_block = if cfg.aliases.is_empty() {
        String::new()
    } else {
        let mut block = String::from("# Aliases\n");
        for (name, cmd) in &cfg.aliases {
            block.push_str(&format!("alias {}={}\n", name, shell_quote(cmd)));
        }
        block
    };
    let theme_note = if external.is_some() {
        // Name the pick's ACTUAL source (run-13 journaled "the oh-my-bash
        // theme" above a bash-it BASH_IT_THEME block — the note must never
        // contradict the loader it sits on).
        format!(
            "# Prompt owned by the {} theme '{}'; PS1 renders via the bash-compatible channel.\n",
            cfg.theme_source_id.as_deref().unwrap_or("external"),
            cfg.theme
        )
    } else {
        String::new()
    };
    format!(
        r#"# Niubash interactive rc - generated by the setup wizard.
# Edit this file with normal Niubash/bash syntax.
# Themes and plugins come from the external ecosystem (oh-my-bash and
# friends); the built-in plugin/theme stack is retired.

{header}
if [ -z "${{HOME:-}}" ] && [ -n "${{USERPROFILE:-}}" ]; then
  case "$USERPROFILE" in
    /[A-Za-z]/*)
      __niubash_home_drive="${{USERPROFILE#/}}"
      __niubash_home_drive="${{__niubash_home_drive%%/*}}"
      __niubash_home_rest="${{USERPROFILE#/$__niubash_home_drive/}}"
      HOME="$__niubash_home_drive:/$__niubash_home_rest"
      ;;
    *)
      HOME="${{USERPROFILE//\\//}}"
      ;;
  esac
  export HOME
fi
unset __niubash_home_drive __niubash_home_rest

{external_block}{alias_block}{theme_note}
# Plugins are declared in ~/.niubash/plugins.toml (the spec); this one
# bootstrap line reconciles on startup — quiet when everything is in sync
# (set NIU_PLUGIN_BOOTSTRAP=off to skip).
# NIU_SHELL is exported by niu itself (its own executable path), so the
# reconcile runs under THIS niu even when PATH still resolves to an older
# install first; outside niu (plain `source ~/.niubashrc`) it falls back
# to whatever `niu` is on PATH.
command -v "${{NIU_SHELL:-niu}}" >/dev/null 2>&1 && "${{NIU_SHELL:-niu}}" plugin sync --bootstrap

# Change things later (nothing here runs automatically):
#   niu plugin discover          see external sources & themes (read-only)
#   niu plugin add <target>      declare + install a plugin source
#   niu plugin sync              reconcile ~/.niubash/plugins.toml with reality
#   niu plugin source trust      review and activate a source's assets
#   niu setup                    re-run this guide
"#,
        header = header,
        external_block = external.unwrap_or_default(),
        alias_block = alias_block,
        theme_note = theme_note,
    )
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', r#"'\''"#))
}

fn write_primary_rc(home: &std::path::Path, rc_content: &str) -> anyhow::Result<Option<PathBuf>> {
    let rc_path = home.join(PRIMARY_RC_FILE);
    let niubash_dir = home.join(".niubash");
    std::fs::create_dir_all(&niubash_dir)?;
    let stamp = timestamp_id();
    let tmp_path = niubash_dir.join(format!(".niubashrc.tmp-{stamp}"));
    std::fs::write(&tmp_path, rc_content)?;

    let backup_path = if rc_path.is_file() {
        let backup_dir = niubash_dir.join("backups");
        std::fs::create_dir_all(&backup_dir)?;
        let backup = backup_dir.join(format!(".niubashrc.{stamp}.bak"));
        std::fs::copy(&rc_path, &backup)?;
        Some(backup)
    } else {
        None
    };

    if rc_path.exists() {
        std::fs::remove_file(&rc_path)?;
    }
    match std::fs::rename(&tmp_path, &rc_path) {
        Ok(()) => {}
        Err(_) => {
            std::fs::copy(&tmp_path, &rc_path)?;
            let _ = std::fs::remove_file(&tmp_path);
        }
    }
    Ok(backup_path)
}

fn timestamp_id() -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    format!("{}-{}", now.as_secs(), now.subsec_millis())
}

/// Built-in Chinese (Simplified) translations for the setup wizard. Keys are
/// the exact English literals passed to `Lang::tr`; anything missing falls
/// back to English. Keep compound lines as one template with the same
/// placeholder order so `format!` args line up in both languages.
fn zh(en: &str) -> Option<&'static str> {
    Some(match en {
        // Welcome
        "Welcome to Niubash" => "欢迎来到 Niubash",
        "A native Rust implementation of bash for Windows \u{2014} no WSL, no MSYS2, no emulation layer." =>
            "Windows 原生 Rust 实现的 bash —— 无需 WSL、MSYS2，没有模拟层。",
        "Reconfigure your interactive prompt/plugins. Existing rc will be backed up." =>
            "重新配置交互提示符/插件。现有 rc 文件会先备份。",
        "Let\u{2019}s get you set up." => "我们开始配置吧。",

        // Environment summary
        "Environment" => "环境",
        "classic console" => "传统控制台",
        "none detected" => "未检测到",
        "terminal" => "终端",
        "nerd font" => "Nerd 字体",
        "command links" => "命令链接",
        "tools" => "工具",
        "detected" => "已安装",
        "not found" => "未检测到",
        "ready" => "就绪",
        "missing" => "缺失",

        // MinTTY note
        "This terminal can't host interactive menus (Git Bash/MinTTY)." =>
            "当前终端（Git Bash/MinTTY）不支持交互菜单。",
        "Applying the 'minimal' preset. Re-run `niu setup` inside" =>
            "已应用 'minimal' 预设。请在 Windows Terminal、cmd 或",
        "Windows Terminal, cmd, or PowerShell for the full wizard." =>
            "PowerShell 中重新运行 `niu setup` 进入完整向导。",

        // Command-link warning
        "WinuxCmd command links look missing (ls/cat/grep/ln)." =>
            "WinuxCmd 命令链接似乎缺失（ls/cat/grep/ln）。",
        "They are created automatically on startup; if Unix commands still" =>
            "它们会在启动时自动创建；若设置完成后 Unix 命令仍",
        #[cfg(windows)]
        "fail after setup, restart niu or run `wpm links rebuild`." =>
            "无法使用，请重启 niu 或运行 `wpm links rebuild`。",

        // Theme gallery step
        // niubash#179 L05-2: keys must match the runtime literals exactly
        // (the gallery Skip row uses a plain hyphen, and the empty-gallery
        // note names the external layer).
        "Skip - keep my current theme" => "跳过 —— 保留当前主题",
        "  · built-in fallback" => "  · 内置保底",
        "  \u{1f3a8}  Pick a theme" => "  \u{1f3a8}  选择主题",
        "  │  external themes come first; entries marked 'built-in fallback' are the safe built-ins\n  \u{2502}  Skip changes nothing" =>
            "  \u{2502}  外部主题排在前面；标注“内置保底”的是安全内置项\n  \u{2502}  跳过则不做任何改动",
        "  │  external themes from your trusted plugin sources; Skip changes nothing" =>
            "  \u{2502}  来自你已信任插件源的外部主题；跳过则不做任何改动",
        "No external themes installed yet - keeping the default look." =>
            "尚未安装外部主题 —— 保持默认外观。",
        "Browse the ecosystem any time with `niu plugin discover` (read-only)." =>
            "随时用 `niu plugin discover` 浏览生态（只读，不安装）。",

        // Plugin collection question (Q2.5)
        "  \u{1f9f0}  Plugin collection?" => "  \u{1f9f0}  插件合集？",
        "default — browse later with `niu plugin recipe list`" =>
            "默认 —— 以后用 `niu plugin recipe list` 再浏览",
        "bash-completion only, no frameworks" => "仅 bash-completion，不含框架",
        "oh-my-bash + its default theme + completions" =>
            "oh-my-bash + 默认主题 + 补全",
        "both frameworks + hooks + curated bash plugins; fzf/starship \
                 are suggested installs (niu downloads nothing)" =>
            "双框架 + 钩子 + 精选 bash 插件；fzf/starship 仅为安装建议（niu 不下载任何东西）",
        "  |  installs stay untrusted until `niu plugin trust`; Skip changes nothing" =>
            "  |  安装后保持未信任，待 `niu plugin trust` 审阅；跳过则不做任何改动",

        // Post-install theme pick (owner ruling 2026-10-03)
        "Trust now" => "现在信任",
        "default — stay untrusted; nothing changes" => "默认 —— 保持未信任，不做任何改动",
        "verify the checksum, then list themes from {}" => "校验 checksum，然后列出 {} 的主题",
        "  \u{1f510}  Trust '{}' now to list its themes?" =>
            "  \u{1f510}  现在信任 '{}' 以列出它的主题？",
        "  \u{1f510}  Trust the freshly installed sources now to list their themes?" =>
            "  \u{1f510}  现在信任新安装的源以列出它们的主题？",
        "  |  this question is the `niu plugin trust` verb; Skip leaves the source untrusted" =>
            "  |  这一问就是 `niu plugin trust` 动作本身；跳过则保持未信任",
        "Theme pick skipped — this run's summary follows" =>
            "已跳过主题选择 —— 接下来显示本次运行的摘要",
        "trusted '{}' — its themes join the catalog" => "已信任 '{}' —— 其主题加入目录",
        "could not trust '{}': {}" => "无法信任 '{}'：{}",
        "no themes appeared after trust — pick later with `niu plugin enable <theme>`" =>
            "信任后没有出现主题 —— 以后用 `niu plugin enable <主题>` 选择",
        "Theme pick skipped — trust later, then pick:" => "已跳过主题选择 —— 以后信任后再选：",
        "then re-run `niu setup` (or `niu plugin enable <theme>`)" =>
            "然后重新运行 `niu setup`（或 `niu plugin enable <主题>`）",
        "current look unchanged" => "当前外观保持不变",
        "keep" => "保留",
        "default" => "默认",
        "rendering preview …" => "预览渲染中 …",
        "(preview unavailable: " => "（预览不可用：",
        "your theme (~/.niubash/themes)" => "你的主题（~/.niubash/themes）",
        "theme (external source, primary)" => "主题（外部源，主选）",
        "renders via the bash-compatible PS1 channel; built-in themes stay as fallback" =>
            "经 bash 兼容 PS1 通道渲染；内置主题作为保底保留",
        "needs a Nerd Font — `niu font` shows install commands (optional)" =>
            "需要 Nerd Font —— `niu font` 给出安装命令（可选）",

        // Completion opt-in
        "Skip" => "跳过",
        "Enable" => "启用",
        "add" => "添加",
        "default — nothing changes" => "默认 —— 不做任何改动",
        "  \u{2328}\u{fe0f}  Extra tab completions for tools on PATH?" =>
            "  \u{2328}\u{fe0f}  为 PATH 上的工具启用更多 Tab 补全？",
        "  \u{2502}  adds the completion packs found on PATH; skip keeps the defaults" =>
            "  \u{2502}  添加在 PATH 上找到的工具的补全包；跳过则保持默认",

        // niu-git single-choice
        "Install" => "安装",
        "Don't ask again" => "不再询问",
        "remember this and stop offering" => "记住这个选择，之后不再提供",
        "default — nothing is installed" => "默认 —— 不安装任何东西",
        "  \u{1f9e9}  niu-git — Windows-native git experience?" =>
            "  \u{1f9e9}  niu-git —— Windows 原生 git 体验？",
        "  \u{2502}  a separate GPLv2 project; your current git keeps working either way" =>
            "  \u{2502}  独立的 GPLv2 项目；无论选不选，现有 git 照常工作",
        "  \u{2502}  a separate GPLv2 project — native Windows git without MSYS" =>
            "  \u{2502}  独立的 GPLv2 项目 —— 无 MSYS 的 Windows 原生 git",
        "niu-git installed" => "niu-git 已安装",
        "niu-git install failed — no other changes were made" =>
            "niu-git 安装失败 —— 其他内容未做任何改动",
        "niu installs nothing itself — run one of the commands above, then restart niu" =>
            "niu 本身不安装任何东西 —— 运行上面的命令之一，然后重启 niu",

        // Summary (labels shared with the environment summary above)
        "Summary" => "配置摘要",
        "theme" => "主题",
        "plugins" => "插件",
        "installs stay untrusted until reviewed" => "安装后保持未信任，待审阅",
        "plugin collection" => "插件合集",
        "installed — review with `niu plugin trust <id>`" =>
            "已安装 —— 用 `niu plugin trust <id>` 审阅启用",
        "plugin spec written — {} source(s) declared; `niu plugin sync` keeps them in sync" =>
            "插件规格已写入 —— 已声明 {} 个源；`niu plugin sync` 保持同步",
        "could not write the plugin spec" => "无法写入插件规格",
        "entries failed — retry with" => "个条目失败 —— 可重试：",
        "failed" => "失败",
        "completions" => "补全",
        "niu-git" => "niu-git",
        "oh-my-bash source" => "oh-my-bash 源",
        "unchanged" => "保持不变",
        "skipped" => "已跳过",
        "don't ask again" => "不再询问",
        "everything else stays untouched" => "其余一切保持原样",

        // Setup journal / undo
        "could not write the setup journal" => "无法写入设置日志",
        "Undo this run:" => "撤销本次设置：",
        "Collection entries failed — retry with:" => "合集条目安装失败 —— 重试：",

        // Confirm + final messages
        "  \u{2705}  Apply this configuration?" => "  \u{2705}  应用此配置？",
        "Apply" => "应用",
        "Cancel" => "取消",
        "Nothing was written." => "未写入任何内容。",
        "Setup cancelled \u{2014} nothing was written." =>
            "设置已取消 —— 未写入任何内容。",
        "Shell rc written to {}" => "Shell 配置已写入 {}",
        "Previous rc backed up to {}" => "原 rc 已备份至 {}",
        "could not write" => "无法写入",

        // Finish screen
        "New config takes effect in new terminals." =>
            "新配置将在新打开的终端生效。",
        "A running niu session applies it at the next prompt; other shells: `source ~/.niubashrc`" =>
            "正在运行的 niu 会话会在下一个提示符自动应用；其他终端请执行 `source ~/.niubashrc`",
        "Change things later:" => "之后想调整：",
        "theme      `niu plugin list`  →  `niu plugin enable <name>`" =>
            "主题       `niu plugin list`  →  `niu plugin enable <名称>`",
        "plugins    `niu plugin list`  ·  `niu plugin enable <name>`" =>
            "插件       `niu plugin list`  ·  `niu plugin enable <名称>`",
        "ecosystem  `niu plugin discover`  (sources & themes, read-only)" =>
            "生态       `niu plugin discover`（插件源与主题，只读）",
        "font / WT  `niu font`  ·  `niu --install-wt-profile`" =>
            "字体/终端  `niu font`  ·  `niu --install-wt-profile`",
        "this guide `niu setup`" => "本向导     `niu setup`",

        // apply_preset
        "unknown preset" => "未知预设",
        "available:" => "可用：",
        "Preset applied:" => "预设已应用：",

        // In-session apply of a finished setup run (niubash#180, shell.rs)
        "\u{21bb}  Applied the new configuration from ~/.niubashrc (`niu setup`)." =>
            "\u{21bb}  已应用新配置 —— ~/.niubashrc（来自 `niu setup`）。",
        "\u{26a0}\u{fe0f}  Could not apply the new configuration \u{2014} run \
         `source ~/.niubashrc` to see the error." =>
            "\u{26a0}\u{fe0f}  应用新配置失败 —— 请执行 `source ~/.niubashrc` 查看错误。",

        // Preset-expansion notes
        "pack '{}' skipped ('{}' not found on PATH)" =>
            "插件包 '{}' 已跳过（PATH 中未找到 '{}'）",
        "theme '{}' needs a Nerd Font \u{2014} using 'classic'" =>
            "主题 '{}' 需要 Nerd Font —— 改用 'classic'",
        "aliases for '{}' skipped (not found on PATH)" =>
            "'{}' 相关别名已跳过（PATH 中未找到）",
        "pack '{}' not in the bundle \u{2014} skipped" =>
            "插件包 '{}' 不在 bundle 中 —— 已跳过",

        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::PROCESS_STATE_LOCK;

    // ── Release-bundle detection (niubash#230 follow-up) ───────────────────

    /// Stage a fake winuxcmd root: `opt/<pkg>/<exe>` payloads plus optional
    /// `usr/bin` shims (winuxcmd.exe hardlinks in the real layout).
    fn stage_bundle_root(temp: &std::path::Path, pkg: &str, exe: &str, shim: Option<&str>) {
        let opt = temp.join("opt").join(pkg);
        std::fs::create_dir_all(&opt).unwrap();
        std::fs::write(opt.join(exe), b"payload").unwrap();
        if let Some(shim) = shim {
            let usr_bin = temp.join("usr").join("bin");
            std::fs::create_dir_all(&usr_bin).unwrap();
            std::fs::write(usr_bin.join(shim), b"shim").unwrap();
        }
    }

    #[test]
    fn bundled_payload_under_opt_counts_as_present() {
        let temp = unique_temp_dir("wizard-bundle-opt");
        stage_bundle_root(&temp, "ripgrep", "rg.exe", None);
        assert_eq!(bundled_components_at(Some(&temp)), vec!["ripgrep"]);
        let _ = std::fs::remove_dir_all(&temp);
    }

    #[test]
    fn bundled_shim_without_payload_counts_as_present() {
        // wpm's layout ships the payload in opt\ but a stripped install may
        // keep only the usr\bin hardlink shim — either alone means the
        // component is usable through the dispatcher.
        let temp = unique_temp_dir("wizard-bundle-shim");
        let usr_bin = temp.join("usr").join("bin");
        std::fs::create_dir_all(&usr_bin).unwrap();
        std::fs::write(usr_bin.join("git.exe"), b"shim").unwrap();
        assert_eq!(bundled_components_at(Some(&temp)), vec!["niugit"]);
        let _ = std::fs::remove_dir_all(&temp);
    }

    #[test]
    fn missing_bundle_and_missing_root_detect_nothing() {
        let temp = unique_temp_dir("wizard-bundle-empty");
        assert!(bundled_components_at(Some(&temp)).is_empty(), "empty root");
        assert!(bundled_components_at(None).is_empty(), "no root at all");
        let _ = std::fs::remove_dir_all(&temp);
    }

    #[test]
    fn bundled_tools_satisfy_probes_with_bundled_source() {
        let probe = EnvProbe::from_parts(BTreeSet::new(), vec!["niugit", "fd"]);
        assert!(
            probe.on_path("git"),
            "bundled niugit satisfies the git probe"
        );
        assert!(probe.on_path("fd"));
        assert!(!probe.on_path("rg"), "rg is not in this fake bundle");
        let names = probe.bundled_tool_names();
        assert!(names.contains("git") && names.contains("fd"));
        assert!(!names.contains("rg"));
    }

    #[test]
    fn system_tools_are_not_shadowed_by_bundle_labels() {
        let mut tools = BTreeSet::new();
        tools.insert("rg".to_string());
        let probe = EnvProbe::from_parts(tools, vec!["ripgrep"]);
        // rg resolves on PATH (system): the bundled label must not duplicate
        // or reannotate it in the summary's tool list.
        assert!(probe.bundled_tool_names().is_empty());
        assert!(probe.on_path("rg"));
    }

    /// The niu-git question is skipped entirely when the release bundle
    /// already ships niu-git — and the fact is recorded so re-runs stay
    /// silent. No install recommendation may surface.
    #[cfg(windows)]
    #[test]
    fn niu_git_question_skipped_when_niugit_is_bundled() {
        let _process_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let temp = unique_temp_dir("wizard-niugit-bundled");
        let home = temp.join("home");
        std::fs::create_dir_all(&home).unwrap();

        let probe = EnvProbe::from_parts(BTreeSet::new(), vec!["niugit"]);
        let mut io = WizardIo::new(false);
        let choice = ask_niu_git(&mut io, &Lang::En, &home, &probe);
        assert_eq!(choice, Some(NiuGitChoice::Skip));
        assert_eq!(read_niu_git_answer(&home).as_deref(), Some("installed"));
        let _ = std::fs::remove_dir_all(&temp);
    }

    /// Without the bundle the question stays offerable: no lasting answer is
    /// written on a plain skip, so a later explicit `niu setup` can still
    /// surface the (never auto-installed) choice.
    #[cfg(windows)]
    #[test]
    fn niu_git_question_still_offered_without_the_bundle() {
        let _process_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let temp = unique_temp_dir("wizard-niugit-absent");
        let home = temp.join("home");
        std::fs::create_dir_all(&home).unwrap();

        let probe = EnvProbe::from_parts(BTreeSet::new(), Vec::new());
        let mut io = WizardIo::new(false);
        let choice = ask_niu_git(&mut io, &Lang::En, &home, &probe);
        assert_eq!(choice, Some(NiuGitChoice::Skip));
        assert_eq!(
            read_niu_git_answer(&home),
            None,
            "a bundle-less skip must not write a lasting answer"
        );
        let _ = std::fs::remove_dir_all(&temp);
    }

    #[test]
    fn display_welcome_side_by_side_renders_without_panic() {
        display_welcome_side_by_side(false, Lang::En);
        display_welcome_side_by_side(true, Lang::Zh);
    }

    #[test]
    fn setup_home_dir_accepts_shell_style_home_env() {
        let _process_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let home = unique_temp_dir("niubash-setup-home").join("home");
        let _home = EnvGuard::set("HOME", &host_to_shell_style_path(&home));
        let _userprofile = EnvGuard::unset("USERPROFILE");

        let resolved = setup_home_dir();
        if cfg!(windows) {
            assert_eq!(display_path(&resolved), display_path(&home));
        } else {
            assert_eq!(display_path(&resolved), display_path(&home));
        }
    }

    fn clean_cfg() -> WizardConfig {
        WizardConfig {
            cwd_style: "home".to_string(),
            completion_style: "column".to_string(),
            ..WizardConfig::default()
        }
    }

    /// niubash#180: every rc rewrite leaves the one-shot handoff marker for
    /// the live session; the session consumes it exactly once, and a stale
    /// marker from before a session started is cleared as its baseline.
    #[test]
    fn rc_rewrite_leaves_a_consumable_apply_pending_marker() {
        let temp = unique_temp_dir("wizard-apply-pending");
        let home = temp.join("home");
        std::fs::create_dir_all(&home).unwrap();

        assert!(!take_apply_pending_marker(&home), "no marker before a run");
        let backup = write_rc_and_mark_done(&home, &clean_cfg(), Lang::En)
            .expect("the rc write must succeed");
        assert!(
            backup.is_none(),
            "no previous rc means no backup: {backup:?}"
        );
        assert!(apply_pending_path(&home).is_file(), "marker written");
        assert!(take_apply_pending_marker(&home), "marker consumed once");
        assert!(
            !take_apply_pending_marker(&home),
            "second take must not fire — a failed apply can never loop the REPL"
        );

        clear_stale_apply_pending_marker(&home);
        assert!(!apply_pending_path(&home).is_file());
        let _ = std::fs::remove_dir_all(&temp);
    }

    #[test]
    fn generated_rc_is_clean_of_the_retired_stack() {
        let rc = generate_rc(&clean_cfg());
        // niubash#145: no built-in plugin/theme machinery in the generated rc.
        assert!(!rc.contains("NIU_PLUGINS="), "{rc}");
        assert!(!rc.contains("NIU_DISABLE_DEFAULT_PLUGINS"), "{rc}");
        assert!(!rc.contains("NIU_THEME="), "{rc}");
        assert!(!rc.contains("NIU_THEME_PLUGIN="), "{rc}");
        assert!(!rc.contains("NIU_PROMPT_SYMBOL="), "{rc}");
        assert!(!rc.contains("oh-my-niu"), "{rc}");
        assert!(!rc.contains("niubash_prompt_use_template"), "{rc}");
        assert!(!rc.contains("NIUBASH="), "{rc}");
        // Display preferences and the HOME bootstrap survive.
        assert!(rc.contains("NIU_PROMPT_CWD_STYLE='home'"), "{rc}");
        assert!(rc.contains("NIU_COMPLETION_STYLE='column'"), "{rc}");
        assert!(rc.contains("USERPROFILE"), "{rc}");
        // The how-to-change-later hints are still there, plus the
        // one-line spec bootstrap (§14.6.3). The bootstrap must run under
        // NIU_SHELL (the running exe) with a bare-`niu` fallback — a bare
        // `niu` alone PATH-shadow-resolves to a stale install and either
        // errors (`unknown plugin subcommand 'sync'`) or reconciles under
        // another vintage's semantics (wt82-L01 V1).
        assert!(rc.contains("niu plugin discover"), "{rc}");
        assert!(rc.contains("niu plugin add <target>"), "{rc}");
        assert!(rc.contains("niu plugin sync"), "{rc}");
        assert!(
            rc.contains(r#"command -v "${NIU_SHELL:-niu}" >/dev/null 2>&1 && "${NIU_SHELL:-niu}" plugin sync --bootstrap"#),
            "{rc}"
        );
        assert!(rc.contains("niu setup"), "{rc}");
    }

    #[test]
    fn generated_rc_writes_aliases() {
        let mut cfg = clean_cfg();
        cfg.aliases = vec![("ll".to_string(), "ls -la".to_string())];
        let rc = generate_rc(&cfg);
        assert!(rc.contains("alias ll='ls -la'"));
    }

    /// niubash#157: the HOME bootstrap's `*)` branch must emit the closed
    /// backslash-to-slash substitution `${USERPROFILE//\\//}` (the form
    /// `.niubashrc.example` carries). A single-backslash `${...//\}` uncloses
    /// the parameter expansion — GNU parse.y:3877 parse_matched_pair() then
    /// fails the whole file at parse time, silently disabling every alias in
    /// the generated rc. The end-to-end parseability guard lives in
    /// tests/startup_files.rs (setup_preset_rc_parses_clean_under_noexec).
    #[test]
    fn generated_rc_home_bootstrap_expansion_is_closed() {
        let rc = generate_rc(&clean_cfg());
        assert!(
            rc.contains(r#"HOME="${USERPROFILE//\\//}""#),
            "HOME bootstrap must rewrite backslashes to slashes, got:{rc}"
        );
        assert!(
            !rc.contains(r#"//\}""#),
            "rc must not contain an unclosed `${{...//}}` expansion:{rc}"
        );
    }

    #[test]
    fn builtin_presets_expand_to_configs() {
        let probe = EnvProbe {
            windows_terminal: false,
            mintty_hint: false,
            nerd_font: false,
            command_links: true,
            tools: BTreeSet::new(),
            bundled: Vec::new(),
        };
        let presets = builtin_presets();
        assert!(presets.len() >= 3);
        assert!(presets.iter().any(|p| p.name == "recommended"));
        for preset in &presets {
            let mut notes = Vec::new();
            let cfg = preset.to_config(&probe, &mut notes, Lang::En);
            assert!(!cfg.completion_style.is_empty());
            assert!(!cfg.cwd_style.is_empty());
        }
        // The recommended preset contributes aliases on a bare install.
        let recommended = presets.iter().find(|p| p.name == "recommended").unwrap();
        let cfg = recommended.to_config(&probe, &mut Vec::new(), Lang::En);
        assert!(cfg.aliases.iter().any(|(name, _)| name == "ll"));
    }

    #[test]
    fn zh_translations_cover_the_wizard_vocab() {
        // Every key here is asserted to translate so a renamed English literal
        // fails loudly instead of silently falling back to English.
        for key in [
            "Welcome to Niubash",
            "  \u{1f3a8}  Pick a theme",
            // niubash#179 L05-2: the test guards the *runtime* literals —
            // the gallery Skip row uses a plain hyphen, and the
            // empty-gallery note names the external layer.
            "Skip - keep my current theme",
            "No external themes installed yet - keeping the default look.",
            "Browse the ecosystem any time with `niu plugin discover` (read-only).",
            "niu installs nothing itself — run one of the commands above, then restart niu",
            "  \u{1f9f0}  Plugin collection?",
            "default — browse later with `niu plugin recipe list`",
            "plugin collection",
            "  \u{1f510}  Trust '{}' now to list its themes?",
            "Trust now",
            "default — stay untrusted; nothing changes",
            "Theme pick skipped — trust later, then pick:",
            "then re-run `niu setup` (or `niu plugin enable <theme>`)",
            "plugin spec written — {} source(s) declared; `niu plugin sync` keeps them in sync",
            "could not write the plugin spec",
            "Collection entries failed — retry with:",
            "  \u{1f9e9}  niu-git — Windows-native git experience?",
            "Apply",
            "Cancel",
            "Change things later:",
            // niubash#180: the finish-screen truth line and the live
            // session's apply notices.
            "New config takes effect in new terminals.",
            "A running niu session applies it at the next prompt; other shells: `source ~/.niubashrc`",
            "\u{21bb}  Applied the new configuration from ~/.niubashrc (`niu setup`).",
            "\u{26a0}\u{fe0f}  Could not apply the new configuration \u{2014} run \
             `source ~/.niubashrc` to see the error.",
        ] {
            assert!(zh(key).is_some(), "missing zh translation for {key:?}");
        }
        // Unknown keys fall back to English verbatim.
        assert_eq!(Lang::Zh.tr("untranslated literal"), "untranslated literal");
    }

    /// The vendored oh-my-bash fixture tree shipped with the repo tests.
    fn omb_fixture_path() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/sources/oh-my-bash")
    }

    fn install_trusted_omb_fixture(root: &std::path::Path) {
        crate::plugins::sources::add_source(crate::plugins::sources::SourceInstallRequest {
            adapter: None,
            origin: omb_fixture_path().to_string_lossy().into_owned(),
            ref_name: None,
            commit: None,
            expected_checksum: None,
            id: None,
            entry: None,
            fetch_budget: None,
        })
        .expect("fixture source add must succeed");
        crate::plugins::sources::trust_source("oh-my-bash").expect("fixture trust must succeed");
        let _ = root;
    }

    #[test]
    fn theme_gallery_lists_trusted_external_sources_only() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let temp = unique_temp_dir("wizard-gallery");
        let root = temp.join("sources");
        let _sources = EnvGuard::set("NIU_PLUGIN_SOURCES_ROOT", &root.to_string_lossy());
        install_trusted_omb_fixture(&root);

        let gallery = theme_gallery();
        assert!(!gallery.entries.is_empty(), "{gallery:?}");
        let position = |name: &str| {
            gallery
                .entries
                .iter()
                .position(|entry| entry.name == name)
                .unwrap_or_else(|| panic!("{name} missing from {gallery:?}"))
        };
        let agnoster = position("agnoster");
        let robbyrussell = position("robbyrussell");
        // Sorted by name, every entry is an external source theme.
        assert!(agnoster < robbyrussell, "{gallery:?}");
        for entry in &gallery.entries {
            assert_eq!(entry.source_id, "oh-my-bash", "{gallery:?}");
        }
        assert_eq!(
            gallery
                .entries
                .iter()
                .filter(|entry| entry.name == "agnoster")
                .count(),
            1,
            "same-name collision must resolve once: {gallery:?}"
        );

        crate::plugins::sources::remove_source("oh-my-bash").unwrap();
        let _ = std::fs::remove_dir_all(&temp);
    }

    /// The vendored bash-it fixture tree shipped with the repo tests.
    fn bash_it_fixture_path() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/sources/bash-it")
    }

    fn install_trusted_bash_it_fixture(root: &std::path::Path) {
        let _ = root;
        crate::plugins::sources::add_source(crate::plugins::sources::SourceInstallRequest {
            adapter: None,
            origin: bash_it_fixture_path().to_string_lossy().into_owned(),
            ref_name: None,
            commit: None,
            expected_checksum: None,
            id: None,
            entry: None,
            fetch_budget: None,
        })
        .expect("bash-it fixture source add must succeed");
        crate::plugins::sources::trust_source("bash-it").expect("bash-it fixture trust");
    }

    /// J5 gap (journey run-13, 2026-10-04), both halves in one fixture pass
    /// (a single install of both frameworks keeps the env-sensitive window
    /// of the suite's Windows env race as short as possible):
    ///
    /// 1. Gallery routing: both fixtures ship a theme named `demox`, and the
    ///    old plain-name sort kept whichever source the registry listed
    ///    first — the preview read "demox - bash-it theme", the rc got a
    ///    BASH_IT_THEME block while the wizard model said oh-my-bash, and
    ///    PS1 stayed the default. The collision must resolve exactly once,
    ///    deterministically, to the primary framework (oh-my-bash).
    /// 2. Loader fidelity (§14.4): whichever source a picked theme came
    ///    from, the rc activates it through THAT framework's own mechanism
    ///    (no shims, no PS1-writing): OSH_THEME + the guarded oh-my-bash.sh
    ///    loader for oh-my-bash picks, BASH_IT_THEME + the guarded
    ///    bash_it.sh loader for bash-it picks, note naming the real source.
    #[test]
    fn theme_pick_routes_and_applies_through_the_owning_framework() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let temp = unique_temp_dir("wizard-routing");
        let root = temp.join("sources");
        let home = temp.join("home");
        std::fs::create_dir_all(&home).unwrap();
        let _sources = EnvGuard::set("NIU_PLUGIN_SOURCES_ROOT", &root.to_string_lossy());
        let _home = EnvGuard::set("HOME", &host_to_shell_style_path(&home));
        let _userprofile = EnvGuard::unset("USERPROFILE");
        // Register bash-it FIRST on purpose: the old bug kept whichever
        // source happened to list first, so the hostile order is the proof.
        install_trusted_bash_it_fixture(&root);
        install_trusted_omb_fixture(&root);

        // (1) The gallery resolves the shared name once, to oh-my-bash.
        let gallery = theme_gallery();
        let shared: Vec<&ThemeGalleryEntry> = gallery
            .entries
            .iter()
            .filter(|entry| entry.name == "demox")
            .collect();
        assert_eq!(shared.len(), 1, "collision must resolve once: {gallery:?}");
        assert_eq!(
            shared[0].source_id, "oh-my-bash",
            "shared names route to the primary framework: {gallery:?}"
        );
        // Both sources still contribute their unique names.
        assert!(
            gallery
                .entries
                .iter()
                .any(|e| e.name == "agnoster" && e.source_id == "oh-my-bash"),
            "{gallery:?}"
        );

        // (2) A pick from either source routes through that framework's own
        // loader block.
        let omb_pick = build_config(&ThemePick::External {
            name: "demox".to_string(),
            source_id: "oh-my-bash".to_string(),
        });
        let rc = generate_rc(&omb_pick);
        assert!(rc.contains("OSH_THEME='demox'"), "{rc}");
        assert!(rc.contains(". \"$OSH/oh-my-bash.sh\""), "{rc}");
        assert!(!rc.contains("BASH_IT_THEME"), "{rc}");
        assert!(
            rc.contains("# Prompt owned by the oh-my-bash theme 'demox'"),
            "the note must name the pick's actual source: {rc}"
        );

        let bash_it_pick = build_config(&ThemePick::External {
            name: "demox".to_string(),
            source_id: "bash-it".to_string(),
        });
        let rc = generate_rc(&bash_it_pick);
        assert!(rc.contains("BASH_IT_THEME='demox'"), "{rc}");
        assert!(rc.contains("bash_it.sh"), "{rc}");
        assert!(!rc.contains("OSH_THEME"), "{rc}");
        assert!(
            rc.contains("# Prompt owned by the bash-it theme 'demox'"),
            "the note must name the pick's actual source: {rc}"
        );

        let _ = crate::plugins::sources::remove_source("oh-my-bash");
        let _ = crate::plugins::sources::remove_source("bash-it");
        let _ = std::fs::remove_dir_all(&temp);
    }

    /// niubash#168 (P0): the theme pick writes BOTH halves — the rc block
    /// through `generate_rc`, and the spec claim through
    /// `record_theme_pick_in_spec` (the helper both the Q1 and the
    /// post-install pick paths call) — so the rc and the spec cannot
    /// disagree about theme ownership. The era-1 stale claim on the shared
    /// name moves with the pick, and every later sync VERIFIES the pick
    /// (byte-stable rc, theme active) instead of re-routing it.
    #[test]
    fn wizard_theme_pick_writes_the_spec_and_survives_sync() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let temp = unique_temp_dir("wizard-pick-spec");
        let root = temp.join("sources");
        let home = temp.join("home");
        std::fs::create_dir_all(&home).unwrap();
        let _sources = EnvGuard::set("NIU_PLUGIN_SOURCES_ROOT", &root.to_string_lossy());
        let _home = EnvGuard::set("HOME", &host_to_shell_style_path(&home));
        let _userprofile = EnvGuard::unset("USERPROFILE");
        let _spec = EnvGuard::set(
            "NIU_PLUGIN_SPEC",
            &home.join(".niubash/plugins.toml").to_string_lossy(),
        );
        // Register bash-it FIRST: the hostile order is the proof (the old
        // bug flipped the rc toward whichever source the spec favored).
        install_trusted_bash_it_fixture(&root);
        install_trusted_omb_fixture(&root);
        // One ranking authority for shared theme names — the gallery dedupe
        // and the spec-layer reconciliation must never disagree.
        assert_eq!(
            crate::plugins::sources::primary_theme_source_rank("oh-my-bash"),
            0
        );
        assert_eq!(
            crate::plugins::sources::primary_theme_source_rank("bash-it"),
            1
        );

        // Era 1: the full-collection era recorded the shared theme under
        // bash-it (registry order of that era).
        crate::plugins::spec::save_spec(&crate::plugins::spec::PluginSpec {
            schema: None,
            sources: vec![crate::plugins::spec::SpecSource {
                target: root.join("bash-it").to_string_lossy().into_owned(),
                id: Some("bash-it".to_string()),
                kind: None,
                ref_name: None,
                theme: Some("demox".to_string()),
                enable: vec![],
            }],
        })
        .unwrap();

        // The wizard pick: the rc block AND the spec claim, in one motion.
        let pick = ThemePick::External {
            name: "demox".to_string(),
            source_id: "oh-my-bash".to_string(),
        };
        let cfg = build_config(&pick);
        let rc = generate_rc(&cfg);
        std::fs::write(home.join(PRIMARY_RC_FILE), &rc).unwrap();
        record_theme_pick_in_spec(&pick);

        // The spec names oh-my-bash (id pin — the ambiguous name is owned
        // unambiguously) and bash-it's theme-only era-1 claim moved away.
        let spec_text =
            std::fs::read_to_string(home.join(".niubash/plugins.toml")).expect("spec written");
        assert!(spec_text.contains("id = 'oh-my-bash'"), "{spec_text}");
        assert!(spec_text.contains("theme = 'demox'"), "{spec_text}");
        assert!(
            !spec_text.contains("id = 'bash-it'"),
            "the theme-only era-1 declaration moved with the pick: {spec_text}"
        );

        // Every later sync verifies the pick: byte-identical rc, theme
        // active, no competing framework block.
        crate::plugins::sync::sync_spec(crate::plugins::sync::SyncOptions::default())
            .expect("sync 1");
        let after = std::fs::read_to_string(home.join(PRIMARY_RC_FILE)).unwrap();
        assert_eq!(after, rc, "the picked theme block never flips");
        assert!(after.contains("OSH_THEME='demox'"), "{after}");
        assert!(!after.contains("BASH_IT_THEME"), "{after}");
        crate::plugins::sync::sync_spec(crate::plugins::sync::SyncOptions::default())
            .expect("sync 2");
        assert_eq!(
            std::fs::read_to_string(home.join(PRIMARY_RC_FILE)).unwrap(),
            rc,
            "byte-stable across sources"
        );

        let _ = crate::plugins::sources::remove_source("oh-my-bash");
        let _ = crate::plugins::sources::remove_source("bash-it");
        let _ = std::fs::remove_dir_all(&temp);
    }

    /// G3 (journey run-13 observation): a collection apply with failures —
    /// or a totally failed apply — must journal what ACTUALLY installed and
    /// feed the finish screen a retry line; a green-looking finish over a
    /// red apply is dishonest. Offline: the collection imports from a local
    /// directory and rides the fixture manager (one healthy asset-theme
    /// entry, one compiled-in entry whose asset the fixture does not carry).
    #[test]
    fn failed_collection_apply_is_journaled_and_reported_honestly() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let temp = unique_temp_dir("wizard-g3-journal");
        let home = temp.join("home");
        std::fs::create_dir_all(&home).unwrap();
        // Full sandbox: apply → assets::enable writes the spec and the rc.
        let _home = EnvGuard::set("HOME", &host_to_shell_style_path(&home));
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
        install_trusted_omb_fixture(&temp.join("sources"));

        let source = temp.join("collection-src");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(
            source.join("niu-collection.toml"),
            "schema = \"niubash:plugin-collection@1\"\nname = \"g3test\"\n\n\
             [[entry]]\nrecipe = \"omb-theme-agnoster\"\n\n\
             [[entry]]\nrecipe = \"omb-theme-90210\"\n",
        )
        .unwrap();
        crate::plugins::distros::import(source.to_string_lossy().as_ref()).expect("import g3test");

        // Partial failure: journal records the collection, no phantom
        // sources, and the failed entry by name.
        let journal = apply_plugin_collection("g3test", Lang::En)
            .expect("a picked collection always journals");
        assert_eq!(journal.name, "g3test");
        assert!(journal.sources.is_empty(), "{:?}", journal.sources);
        assert_eq!(journal.failed, ["omb-theme-90210"], "{:?}", journal.failed);
        let full = SetupJournal {
            collection: Some(journal),
            ..SetupJournal::default()
        };
        write_setup_journal(&home, &full);
        let text = std::fs::read_to_string(home.join(".niubash/setup-journal.toml"))
            .expect("journal written");
        assert!(text.contains("collection = 'g3test'"), "{text}");
        assert!(
            !text.contains("collection_sources"),
            "no source landed; the journal must not invent any: {text}"
        );
        assert!(
            text.contains("collection_failed = ['omb-theme-90210']"),
            "{text}"
        );
        // The finish screen's retry block names the exact verb.
        let retry = setup_retry_lines(&full);
        assert_eq!(retry.len(), 1, "{retry:?}");
        assert!(
            retry[0].starts_with("niu plugin distro apply g3test"),
            "{}",
            retry[0]
        );
        // A healthy apply prints no retry line.
        let healthy = SetupJournal {
            collection: Some(CollectionJournal {
                name: "g3test".to_string(),
                sources: vec!["oh-my-bash".to_string()],
                failed: Vec::new(),
            }),
            ..SetupJournal::default()
        };
        assert!(setup_retry_lines(&healthy).is_empty());

        // Total failure (unknown collection): still journaled, still retried.
        let failed_apply = apply_plugin_collection("no-such-collection", Lang::En)
            .expect("even a failed apply journals itself");
        assert_eq!(failed_apply.name, "no-such-collection");
        assert!(failed_apply.sources.is_empty());
        assert_eq!(failed_apply.failed.len(), 1, "{:?}", failed_apply.failed);
        assert!(
            failed_apply.failed[0].starts_with("apply failed:"),
            "{}",
            failed_apply.failed[0]
        );
        let retry = setup_retry_lines(&SetupJournal {
            collection: Some(failed_apply),
            ..SetupJournal::default()
        });
        assert!(
            retry
                .iter()
                .any(|line| line.contains("niu plugin distro apply no-such-collection")),
            "{retry:?}"
        );

        let _ = crate::plugins::sources::remove_source("oh-my-bash");
        let _ = std::fs::remove_dir_all(&temp);
    }

    #[test]
    fn wizard_skip_answers_leave_zero_rc_overrides() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let temp = unique_temp_dir("wizard-skip");
        let home = temp.join("home");
        std::fs::create_dir_all(&home).unwrap();
        let _home = EnvGuard::set("HOME", &host_to_shell_style_path(&home));
        let _userprofile = EnvGuard::unset("USERPROFILE");

        // Skipped every question, first run (no previous rc): the rc the
        // wizard writes must not override anything.
        let cfg = build_config(&ThemePick::Keep);
        let rc = generate_rc(&cfg);
        assert!(!rc.contains("NIU_PLUGINS="), "{rc}");
        assert!(!rc.contains("NIU_DISABLE_DEFAULT_PLUGINS=1"), "{rc}");
        assert!(!rc.contains("NIU_THEME="), "{rc}");
        assert!(!rc.contains("OSH_THEME="), "{rc}");
        assert!(!rc.contains("niubash_prompt_use_template"), "{rc}");
        assert!(!rc.contains("alias "), "{rc}");
        assert!(!rc.contains("wpm"), "{rc}");
        // The how-to-change-later hints are still there.
        assert!(rc.contains("niu plugin discover"), "{rc}");
        assert!(rc.contains("niu setup"), "{rc}");
        let _ = std::fs::remove_dir_all(&temp);
    }

    #[test]
    fn wizard_external_theme_pick_writes_guarded_loader() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let temp = unique_temp_dir("wizard-omb-theme");
        let root = temp.join("sources");
        let _sources = EnvGuard::set("NIU_PLUGIN_SOURCES_ROOT", &root.to_string_lossy());
        install_trusted_omb_fixture(&root);

        let cfg = WizardConfig {
            theme: "robbyrussell".to_string(),
            theme_source_id: Some("oh-my-bash".to_string()),
            cwd_style: "home".to_string(),
            completion_style: "column".to_string(),
            ..WizardConfig::default()
        };
        let rc = generate_rc(&cfg);
        // §3.2/§3.3: OSH_THEME + NIU_THEME_SOURCE=omb + the guarded loader,
        // and no native theme pair or template override.
        assert!(rc.contains("OSH_THEME='robbyrussell'"), "{rc}");
        assert!(rc.contains("NIU_THEME_SOURCE=omb"), "{rc}");
        assert!(rc.contains("if [ -r "), "{rc}");
        assert!(rc.contains(". \"$OSH/oh-my-bash.sh\""), "{rc}");
        assert!(
            rc.contains("${NIU_PLUGIN_SOURCES_ROOT:-$HOME/.niubash/sources}/oh-my-bash"),
            "{rc}"
        );
        assert!(rc.contains("fallback stays active when absent"), "{rc}");
        // §14.5 rc order contract: floor config (NIU_* display knobs) lands
        // early; the framework enable block loads after it, so a framework
        // that claims PS1 always runs on top of — never below — the floor.
        let floor_pos = rc.find("NIU_PROMPT_CWD_STYLE=").expect("floor knob");
        let block_pos = rc.find(">>> niu source oh-my-bash").expect("managed block");
        assert!(
            floor_pos < block_pos,
            "floor config must precede the framework block:\n{rc}"
        );
        // The wizard-written block is the managed block: markers present so
        // `niu plugin enable/disable` can edit it surgically.
        assert!(
            rc.contains(">>> niu source oh-my-bash (managed by `niu plugin enable/disable`) >>>"),
            "{rc}"
        );
        assert!(!rc.contains("NIU_THEME="), "{rc}");
        assert!(!rc.contains("NIU_THEME_PLUGIN="), "{rc}");
        assert!(!rc.contains("niubash_prompt_use_template"), "{rc}");
        assert!(rc.contains("bash-compatible channel"), "{rc}");

        crate::plugins::sources::remove_source("oh-my-bash").unwrap();
        let _ = std::fs::remove_dir_all(&temp);
    }

    /// The state `apply_plugin_collection` leaves behind, without any
    /// network: the fixture source installed UNTRUSTED from a local path.
    fn install_untrusted_omb_fixture() {
        crate::plugins::sources::add_source(crate::plugins::sources::SourceInstallRequest {
            adapter: None,
            origin: omb_fixture_path().to_string_lossy().into_owned(),
            ref_name: None,
            commit: None,
            expected_checksum: None,
            id: None,
            entry: None,
            fetch_budget: None,
        })
        .expect("fixture source add must succeed");
    }

    fn source_is_trusted(id: &str) -> bool {
        crate::plugins::sources::read_source_registry()
            .into_iter()
            .find(|record| record.id == id)
            .map(|record| record.trusted)
            .unwrap_or(false)
    }

    /// (a) recommended + trust-yes + theme picked: the same run trusts the
    /// fresh source and rewrites the rc through the guarded activation
    /// block — journal/undo cover the pick exactly like a Q1 pick.
    #[test]
    fn post_install_pick_trusts_then_writes_the_guarded_theme_block() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let temp = unique_temp_dir("wizard-post-install");
        let root = temp.join("sources");
        let home = temp.join("home");
        std::fs::create_dir_all(&home).unwrap();
        let _sources = EnvGuard::set("NIU_PLUGIN_SOURCES_ROOT", &root.to_string_lossy());
        let _home = EnvGuard::set("HOME", &host_to_shell_style_path(&home));
        let _userprofile = EnvGuard::unset("USERPROFILE");
        install_untrusted_omb_fixture();

        // Post-apply wizard state: rc already written theme-less, cfg from
        // build_config(Keep), collection journal naming the fresh source.
        let mut cfg = build_config(&ThemePick::Keep);
        std::fs::write(home.join(PRIMARY_RC_FILE), generate_rc(&cfg)).unwrap();
        let candidates = post_install_theme_candidates(Some(&CollectionJournal {
            name: "recommended".to_string(),
            sources: vec!["oh-my-bash".to_string()],
            failed: Vec::new(),
        }));
        assert_eq!(candidates, vec!["oh-my-bash".to_string()], "{candidates:?}");

        // Answers (0-based indices, like every choice_inner return): Trust
        // now (1), then the gallery's first theme — the fixture lists
        // agnoster + robbyrussell sorted, so Skip=0, agnoster=1.
        let mut io = WizardIo::scripted(&[1, 1]);
        let picked = run_post_install_theme_pick(&mut io, Lang::En, &home, &candidates, &mut cfg);
        assert_eq!(
            picked,
            Some(("agnoster".to_string(), "oh-my-bash".to_string())),
            "{picked:?}"
        );
        assert_eq!(cfg.theme, "agnoster");
        assert_eq!(cfg.theme_source_id.as_deref(), Some("oh-my-bash"));
        assert!(
            source_is_trusted("oh-my-bash"),
            "the pick must have trusted it"
        );

        // The rc on disk carries the same guarded block a Q1 pick writes.
        let rc = std::fs::read_to_string(home.join(PRIMARY_RC_FILE)).unwrap();
        assert!(rc.contains("OSH_THEME='agnoster'"), "{rc}");
        assert!(rc.contains("NIU_THEME_SOURCE=omb"), "{rc}");
        assert!(rc.contains(">>> niu source oh-my-bash"), "{rc}");
        assert!(rc.contains("fallback stays active when absent"), "{rc}");

        // One journal + undo lines for both the collection and the pick.
        let journal = SetupJournal {
            rc_backup: None,
            rc_created: true,
            theme: picked.map(|(name, source)| (name, source)),
            preset: None,
            niu_git: None,
            collection: Some(CollectionJournal {
                name: "recommended".to_string(),
                sources: vec!["oh-my-bash".to_string()],
                failed: Vec::new(),
            }),
        };
        let undo = setup_undo_lines(&home, &journal);
        assert!(
            undo.iter()
                .any(|line| line.contains("niu plugin disable agnoster")),
            "{undo:?}"
        );
        assert!(
            undo.iter()
                .any(|line| line.contains("niu plugin source remove oh-my-bash")),
            "{undo:?}"
        );

        crate::plugins::sources::remove_source("oh-my-bash").unwrap();
        let _ = std::fs::remove_dir_all(&temp);
    }

    /// (b) trust declined: no theme question at all, the hint is the only
    /// output, and the rc/registry stay exactly as the collection apply
    /// left them.
    #[test]
    fn post_install_declined_trust_changes_nothing() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let temp = unique_temp_dir("wizard-post-decline");
        let root = temp.join("sources");
        let home = temp.join("home");
        std::fs::create_dir_all(&home).unwrap();
        let _sources = EnvGuard::set("NIU_PLUGIN_SOURCES_ROOT", &root.to_string_lossy());
        let _home = EnvGuard::set("HOME", &host_to_shell_style_path(&home));
        let _userprofile = EnvGuard::unset("USERPROFILE");
        install_untrusted_omb_fixture();

        let mut cfg = build_config(&ThemePick::Keep);
        std::fs::write(home.join(PRIMARY_RC_FILE), generate_rc(&cfg)).unwrap();
        let rc_before = std::fs::read_to_string(home.join(PRIMARY_RC_FILE)).unwrap();
        let candidates = post_install_theme_candidates(Some(&CollectionJournal {
            name: "recommended".to_string(),
            sources: vec!["oh-my-bash".to_string()],
            failed: Vec::new(),
        }));

        // Skip (0): the default answer — the wizard convention that every
        // question defaults to "no change".
        let mut io = WizardIo::scripted(&[0]);
        let picked = run_post_install_theme_pick(&mut io, Lang::En, &home, &candidates, &mut cfg);
        assert!(picked.is_none(), "{picked:?}");
        assert!(cfg.theme.is_empty());
        assert!(cfg.theme_source_id.is_none());
        assert!(!source_is_trusted("oh-my-bash"));
        assert_eq!(
            std::fs::read_to_string(home.join(PRIMARY_RC_FILE)).unwrap(),
            rc_before,
            "the rc must stay byte-identical when trust is declined"
        );
        // Exactly one question was asked (the trust one) — no gallery.
        #[cfg(test)]
        assert!(io.script.is_empty(), "the theme question must not appear");

        // The non-interactive default is the same Skip.
        let mut io = WizardIo::new(false);
        assert!(
            run_post_install_theme_pick(&mut io, Lang::En, &home, &candidates, &mut cfg).is_none()
        );
        assert!(!source_is_trusted("oh-my-bash"));

        // Ctrl-C after Apply skips the pick too — state is already written,
        // so the flow must return (the finish screen follows), not cancel.
        let mut io = WizardIo::scripted(&[usize::MAX]);
        assert!(
            run_post_install_theme_pick(&mut io, Lang::En, &home, &candidates, &mut cfg).is_none()
        );
        assert_eq!(
            std::fs::read_to_string(home.join(PRIMARY_RC_FILE)).unwrap(),
            rc_before
        );

        crate::plugins::sources::remove_source("oh-my-bash").unwrap();
        let _ = std::fs::remove_dir_all(&temp);
    }

    /// F4 (1.3.1): a collection apply ends SPEC-MANAGED. The journey state
    /// after the post-install theme pick — trusted oh-my-bash with the
    /// guarded theme block in the rc, no spec — is declared by the same
    /// adoption `niu plugin sync --adopt` runs, so a fresh wizard user
    /// never lands in the 1.3.0 nag state and no migration verb is needed.
    #[test]
    fn collection_apply_ends_spec_managed() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let temp = unique_temp_dir("wizard-spec-managed");
        let root = temp.join("sources");
        let home = temp.join("home");
        std::fs::create_dir_all(&home).unwrap();
        let _sources = EnvGuard::set("NIU_PLUGIN_SOURCES_ROOT", &root.to_string_lossy());
        let _home = EnvGuard::set("HOME", &host_to_shell_style_path(&home));
        let _userprofile = EnvGuard::unset("USERPROFILE");
        let _spec = EnvGuard::set(
            "NIU_PLUGIN_SPEC",
            &home.join(".niubash/plugins.toml").to_string_lossy(),
        );

        // The journey's end state: collection installed the fixture
        // (untrusted), the trust question trusted it, the theme pick wrote
        // the guarded block through generate_rc.
        install_untrusted_omb_fixture();
        crate::plugins::sources::trust_source("oh-my-bash").expect("fixture trust");
        let cfg = WizardConfig {
            theme: "agnoster".to_string(),
            theme_source_id: Some("oh-my-bash".to_string()),
            ..WizardConfig::default()
        };
        std::fs::write(home.join(PRIMARY_RC_FILE), generate_rc(&cfg)).unwrap();

        adopt_installed_sources_into_spec(Lang::En);

        let spec_text = std::fs::read_to_string(home.join(".niubash/plugins.toml"))
            .expect("the wizard wrote the spec");
        assert!(spec_text.contains("id = 'oh-my-bash'"), "{spec_text}");
        assert!(spec_text.contains("theme = 'agnoster'"), "{spec_text}");
        // The settled state round-trips: a plain sync is a no-op for the rc.
        let rc_before = std::fs::read_to_string(home.join(PRIMARY_RC_FILE)).unwrap();
        let report =
            crate::plugins::sync::sync_spec(crate::plugins::sync::SyncOptions::default()).unwrap();
        assert!(report.clean, "{:?}", report);
        assert_eq!(
            std::fs::read_to_string(home.join(PRIMARY_RC_FILE)).unwrap(),
            rc_before,
            "sync after the wizard must not move the rc"
        );

        crate::plugins::sources::remove_source("oh-my-bash").unwrap();
        let _ = std::fs::remove_dir_all(&temp);
    }

    /// (c) minimal / Skip collections ask nothing: no collection, a failed
    /// apply, or an apply whose sources bear no themes all stay silent.
    #[test]
    fn post_install_candidates_stay_empty_for_minimal_or_skipped_runs() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let temp = unique_temp_dir("wizard-post-minimal");
        let root = temp.join("sources");
        let _sources = EnvGuard::set("NIU_PLUGIN_SOURCES_ROOT", &root.to_string_lossy());

        assert!(
            post_install_theme_candidates(None).is_empty(),
            "Skip (or a failed apply) must never trigger the question"
        );

        // A minimal-style apply lands bash-completion — no theme assets.
        crate::plugins::sources::add_source(crate::plugins::sources::SourceInstallRequest {
            adapter: None,
            origin: PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../tests/fixtures/sources/bash-completion")
                .to_string_lossy()
                .into_owned(),
            ref_name: None,
            commit: None,
            expected_checksum: None,
            id: None,
            entry: None,
            fetch_budget: None,
        })
        .expect("bash-completion fixture add must succeed");
        let candidates = post_install_theme_candidates(Some(&CollectionJournal {
            name: "minimal".to_string(),
            sources: vec!["bash-completion".to_string()],
            failed: Vec::new(),
        }));
        assert!(candidates.is_empty(), "{candidates:?}");

        crate::plugins::sources::remove_source("bash-completion").unwrap();
        let _ = std::fs::remove_dir_all(&temp);
    }

    #[test]
    fn setup_journal_records_entries_and_undo_commands() {
        let temp = unique_temp_dir("setup-journal");
        let backup = temp.join("backups/.niubashrc.1-2.bak");
        let journal = SetupJournal {
            rc_backup: Some(backup.clone()),
            rc_created: false,
            theme: Some(("robbyrussell".to_string(), "oh-my-bash".to_string())),
            preset: None,
            niu_git: None,
            collection: Some(CollectionJournal {
                name: "recommended".to_string(),
                sources: vec!["oh-my-bash".to_string()],
                failed: Vec::new(),
            }),
        };
        write_setup_journal(&temp, &journal);
        let text = std::fs::read_to_string(setup_journal_path(&temp)).unwrap();
        assert!(text.contains(SETUP_JOURNAL_SCHEMA), "{text}");
        assert!(text.contains("rc_backup = "), "{text}");
        assert!(text.contains("theme = 'robbyrussell'"), "{text}");
        assert!(text.contains("theme_source = 'oh-my-bash'"), "{text}");
        assert!(text.contains("collection = 'recommended'"), "{text}");
        assert!(
            text.contains("collection_sources = ['oh-my-bash']"),
            "{text}"
        );

        // One undo command per entry: rc restore, theme disable, and the
        // source-removal hint (deduplicated: the theme and the collection
        // name the same source, so it prints ONCE — niubash#179 L02-2;
        // executable-tool entries install nothing, so they have no undo).
        let undo = setup_undo_lines(&temp, &journal);
        assert!(undo.len() == 3, "{undo:?}");
        assert!(undo[0].starts_with("cp "), "{undo:?}");
        assert!(undo[0].contains(".niubashrc"), "{undo:?}");
        assert!(
            undo[1].contains("niu plugin disable robbyrussell"),
            "{undo:?}"
        );
        assert!(
            undo[2].contains("niu plugin source remove oh-my-bash"),
            "{undo:?}"
        );
        assert!(
            undo.iter()
                .filter(|line| line.contains("niu plugin source remove oh-my-bash"))
                .count()
                == 1,
            "duplicate source-removal receipt (L02-2): {undo:?}"
        );
        assert!(
            !undo.iter().any(|line| line.contains("niu plugin tool")),
            "retracted tool verbs must not appear in undo lines: {undo:?}"
        );

        // A skip run (no theme, no backup) undoes nothing.
        let empty = SetupJournal::default();
        assert!(setup_undo_lines(&temp, &empty).is_empty());

        // Fresh install (niubash#179 L02-1): no previous rc existed, so the
        // receipt covers the write itself — remove the generated rc and the
        // setup-done marker; the theme's undo lines still apply.
        let fresh = SetupJournal {
            rc_backup: None,
            rc_created: true,
            theme: Some(("robbyrussell".to_string(), "oh-my-bash".to_string())),
            preset: None,
            niu_git: None,
            collection: None,
        };
        let undo = setup_undo_lines(&temp, &fresh);
        assert!(undo.len() == 4, "{undo:?}");
        assert!(
            undo[0].starts_with("rm ") && undo[0].contains(".niubashrc"),
            "{undo:?}"
        );
        assert!(
            undo[1].starts_with("rm ") && undo[1].contains(".setup-done"),
            "{undo:?}"
        );
        assert!(
            undo[2].contains("niu plugin disable robbyrussell"),
            "{undo:?}"
        );
        assert!(
            undo[3].contains("niu plugin source remove oh-my-bash"),
            "{undo:?}"
        );
        write_setup_journal(&temp, &fresh);
        let text = std::fs::read_to_string(setup_journal_path(&temp)).unwrap();
        assert!(text.contains("rc_created = true"), "{text}");
        let _ = std::fs::remove_dir_all(&temp);
    }

    #[test]
    #[cfg(windows)]
    fn niu_git_answer_round_trip_gates_the_question() {
        let temp = unique_temp_dir("niu-git-answer");
        assert!(
            read_niu_git_answer(&temp).is_none(),
            "no answer recorded yet — the wizard may offer the choice"
        );
        write_niu_git_answer(&temp, "never");
        assert_eq!(read_niu_git_answer(&temp).as_deref(), Some("never"));
        let text = std::fs::read_to_string(wizard_answers_path(&temp)).unwrap();
        assert!(text.contains("niu_git = \"never\""), "{text}");
        assert!(text.contains(WIZARD_ANSWERS_SCHEMA), "{text}");
        let _ = std::fs::remove_dir_all(&temp);
    }

    #[test]
    fn current_theme_pick_reads_existing_rc() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _theme_env = EnvGuard::unset("NIU_THEME");
        let temp = unique_temp_dir("wizard-current-theme");
        std::fs::create_dir_all(&temp).unwrap();
        assert_eq!(current_theme_pick(&temp), ThemePick::Keep);

        std::fs::write(temp.join(PRIMARY_RC_FILE), "NIU_THEME='classic'\n").unwrap();
        // The retired NIU_THEME channel is ignored (niubash#145).
        assert_eq!(current_theme_pick(&temp), ThemePick::Keep);

        std::fs::write(
            temp.join(PRIMARY_RC_FILE),
            "OSH_THEME='robbyrussell'\nNIU_THEME_SOURCE=omb\n",
        )
        .unwrap();
        assert_eq!(
            current_theme_pick(&temp),
            ThemePick::External {
                name: "robbyrussell".to_string(),
                source_id: "oh-my-bash".to_string()
            }
        );
        let _ = std::fs::remove_dir_all(&temp);
    }

    fn unique_temp_dir(prefix: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("{}-{}-{}", prefix, std::process::id(), nanos))
    }

    fn host_to_shell_style_path(path: &std::path::Path) -> String {
        let display = display_path(path);
        if cfg!(windows) && display.len() >= 3 && display.as_bytes()[1] == b':' {
            let drive = (display.as_bytes()[0] as char).to_ascii_lowercase();
            format!("/{drive}/{}", &display[3..])
        } else {
            display
        }
    }

    fn display_path(path: &std::path::Path) -> String {
        path.to_string_lossy().replace('\\', "/")
    }

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

        fn unset(name: &'static str) -> Self {
            let previous = std::env::var_os(name);
            std::env::remove_var(name);
            Self { name, previous }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            if let Some(previous) = &self.previous {
                std::env::set_var(self.name, previous);
            } else {
                std::env::remove_var(self.name);
            }
        }
    }
}
