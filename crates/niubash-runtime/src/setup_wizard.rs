//! First-run setup wizard.
//!
//! Guides the user through initial interactive configuration, then writes a
//! normal shell rc file.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::interactive_menu::{self, pad_display, Selection};
use crate::path_utils::shell_home_dir;
use crate::theme;

const PRIMARY_RC_FILE: &str = ".niubashrc";
const COMPAT_RC_FILE: &str = ".winuxshrc";
const SETUP_DONE_FILE: &str = ".setup-done";

/// Companion tools offered through wpm on first run, grouped into cumulative
/// tiers so the user picks one bundle instead of per-tool questions. Each
/// entry is a `(binary name on PATH, wpm package name)` pair — they differ
/// for ripgrep, whose binary is `rg`.
const TOOL_TIERS: &[&[(&str, &str)]] = &[
    // Essentials: the daily-driver modern replacements.
    &[
        ("eza", "eza"),
        ("fd", "fd"),
        ("rg", "ripgrep"),
        ("fzf", "fzf"),
        ("bat", "bat"),
    ],
    // Modern CLI: navigation, disk usage, diff, search/replace, processes.
    &[
        ("zoxide", "zoxide"),
        ("dust", "dust"),
        ("duf", "duf"),
        ("delta", "delta"),
        ("sd", "sd"),
        ("procs", "procs"),
    ],
    // Everything: history, monitors, TUI helpers, data tools.
    &[
        ("atuin", "atuin"),
        ("bottom", "bottom"),
        ("lazygit", "lazygit"),
        ("jq", "jq"),
        ("yq", "yq"),
        ("xh", "xh"),
        ("hyperfine", "hyperfine"),
        ("tokei", "tokei"),
        ("glow", "glow"),
        ("navi", "navi"),
        ("watchexec", "watchexec"),
    ],
];

/// Tools probed on PATH during preflight; drives conditional packs/aliases.
const PROBED_TOOLS: &[&str] = &[
    "git", "fzf", "eza", "bat", "starship", "zoxide", "fd", "rg", "dust", "duf", "erd", "direnv",
    "kubectl", "docker", "npm", "thefuck", "wpm",
];

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

/// Everything the wizard can write into `~/.niubashrc`. Presets fill this in
/// one shot; the custom flow fills it question by question.
#[derive(Debug, Clone, Default)]
pub struct WizardConfig {
    pub theme: String,
    pub prompt_style: String,
    pub right_prompt: String,
    pub symbol: String,
    pub cwd_style: String,
    pub prompt_enabled: bool,
    pub git_enabled: bool,
    pub git_backend: GitBackend,
    pub segment_preset: Option<String>,
    pub completion_style: String,
    pub plugins: Vec<String>,
    /// Extra `alias name='cmd'` lines appended to the generated rc.
    pub aliases: Vec<(String, String)>,
}

/// A curated setup preset. Built-ins ship in the binary; the oh-my-niu bundle
/// may drop additional `presets/*.toml` files that extend or override them.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(default)]
pub struct Preset {
    pub name: String,
    pub summary: String,
    pub requires_nerd_font: bool,
    pub theme: String,
    pub prompt_symbol: String,
    pub prompt_style: String,
    pub right_prompt: String,
    pub cwd_style: String,
    pub completion_style: String,
    pub packs: Vec<String>,
    /// Aliases always written into the rc.
    pub aliases: BTreeMap<String, String>,
    /// Pack name -> binary that must exist on PATH for the pack to enable.
    pub conditional_packs: BTreeMap<String, String>,
    /// Binary -> aliases written only when the binary is on PATH.
    pub conditional_aliases: BTreeMap<String, BTreeMap<String, String>>,
    /// Pre-selected answer for the starship question when starship is found.
    pub starship_default: bool,
}

impl Default for Preset {
    fn default() -> Self {
        Preset {
            name: String::new(),
            summary: String::new(),
            requires_nerd_font: false,
            theme: "classic".into(),
            prompt_symbol: ">".into(),
            prompt_style: "minimal".into(),
            right_prompt: "off".into(),
            cwd_style: "home".into(),
            completion_style: "ide".into(),
            packs: Vec::new(),
            aliases: BTreeMap::new(),
            conditional_packs: BTreeMap::new(),
            conditional_aliases: BTreeMap::new(),
            starship_default: false,
        }
    }
}

/// Environment facts collected once, before any question is asked.
struct EnvProbe {
    windows_terminal: bool,
    mintty_hint: bool,
    nerd_font: bool,
    command_links: bool,
    /// Names from `PROBED_TOOLS` that resolved on PATH.
    tools: BTreeSet<String>,
}

impl EnvProbe {
    fn collect() -> Self {
        let mut tools: BTreeSet<String> = PROBED_TOOLS
            .iter()
            .filter(|tool| on_path(tool))
            .map(|tool| tool.to_string())
            .collect();
        // wpm is also usable through `winuxcmd.exe wpm` without a command link.
        if wpm_available() {
            tools.insert("wpm".to_string());
        }
        EnvProbe {
            windows_terminal: std::env::var_os("WT_SESSION").is_some(),
            mintty_hint: std::env::var_os("MSYSTEM").is_some()
                || std::env::var("TERM_PROGRAM")
                    .map(|v| v.eq_ignore_ascii_case("mintty"))
                    .unwrap_or(false),
            nerd_font: crate::fonts::nerd_font_installed(),
            command_links: crate::winuxcmd::command_links_ready(),
            tools,
        }
    }

    fn on_path(&self, tool: &str) -> bool {
        self.tools.contains(tool)
    }

    fn print_summary(&self, lang: Lang) {
        let terminal = if self.windows_terminal {
            "Windows Terminal"
        } else {
            lang.tr("classic console")
        };
        let tools = if self.tools.is_empty() {
            lang.tr("none detected").to_string()
        } else {
            self.tools.iter().cloned().collect::<Vec<_>>().join(" ")
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
    }
}

/// Interactive question driver with fast-forward and abort handling.
///
/// Esc on any question fast-forwards: every remaining question silently takes
/// its default and the flow lands on the summary. Ctrl-C aborts the wizard.
struct WizardIo {
    interactive: bool,
    fast_forward: bool,
    lang: Lang,
}

impl WizardIo {
    fn new(interactive: bool, lang: Lang) -> Self {
        WizardIo {
            interactive,
            fast_forward: false,
            lang,
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
        preview: &dyn Fn(usize) -> Vec<String>,
    ) -> Option<usize> {
        self.choice_inner(label, default_idx, options, help, Some(preview))
    }

    fn choice_inner(
        &mut self,
        label: &str,
        default_idx: usize,
        options: &[&str],
        help: &str,
        preview: Option<&dyn Fn(usize) -> Vec<String>>,
    ) -> Option<usize> {
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

    fn yn(&mut self, label: &str, default: bool) -> Option<bool> {
        let yes = self.lang.tr("Yes");
        let no = self.lang.tr("No");
        let options: [&str; 2] = if default { [yes, no] } else { [no, yes] };
        self.choice(label, 0, &options, "")
            .map(|idx| options[idx] == yes)
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
    let logo_str = crate::logo::render_logo_to_string(logo_cols);
    let logo_lines: Vec<String> = logo_str.lines().map(String::from).collect();

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
    let mut io = WizardIo::new(crate::terminal::stdio_is_interactive(), lang);

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
        let cfg = preset.to_config(&probe, probe.nerd_font, &mut Vec::new(), lang);
        write_rc_and_mark_done(&home, &cfg, lang)?;
        return Ok(());
    }

    // --- Nerd Font capability ---
    let mut nf_capable = probe.nerd_font;
    let mut installed_font: Option<String> = None;
    if !nf_capable {
        println!();
        println!(
            "  \u{2502}  {}  \u{e0b0}\u{e0b2}  \u{f0e7}  \u{f120}  \u{276f}",
            t.tr("Glyph sample:")
        );
        let mut font_labels = crate::fonts::menu_labels();
        if let Some(first) = font_labels.first_mut() {
            first.push_str(t.tr("  (recommended)"));
        }
        font_labels.push(t.tr("Skip \u{2014} keep my current font").to_string());
        let font_refs: Vec<&str> = font_labels.iter().map(String::as_str).collect();
        let font_idx = ask!(io.choice(
            t.tr("  \u{1f5a4}\u{fe0f}  Install a Nerd Font for icon-rich themes?"),
            0,
            &font_refs,
            t.tr("  \u{2502}  downloads from nerd-fonts and installs per-user \u{2014} no admin needed"),
        ));
        if font_idx < crate::fonts::FONT_OPTIONS.len() {
            let font = &crate::fonts::FONT_OPTIONS[font_idx];
            match crate::fonts::install(font) {
                Ok(installed) => {
                    nf_capable = true;
                    installed_font = Some(installed.face.to_string());
                    println!(
                        "  \u{2705}  {}",
                        fill(
                            t.tr("Installed {} ({} files)"),
                            &[&font.label, &installed.files]
                        )
                    );
                    if probe.windows_terminal {
                        try_set_wt_profile_font(installed.face);
                        println!(
                            "  \u{2502}  {}",
                            fill(
                                t.tr("Niubash tabs in Windows Terminal will use '{}'."),
                                &[&installed.face]
                            )
                        );
                    } else {
                        println!(
                            "  \u{2502}  {}",
                            fill(
                                t.tr("Now set your terminal font to '{}'."),
                                &[&installed.face]
                            )
                        );
                    }
                }
                Err(err) => println!(
                    "  \u{26a0}\u{fe0f}  {} {err:#}",
                    t.tr("Font install failed:")
                ),
            }
        }
        if !nf_capable {
            nf_capable = ask!(io.yn(
                t.tr("  \u{1f524}  Do the glyphs above render correctly (not boxes)?"),
                false
            ));
            if !nf_capable {
                println!(
                    "  \u{2502}  {}",
                    t.tr("Nerd-Font themes fall back to 'classic' for now.")
                );
                println!(
                    "  \u{2502}  {}",
                    t.tr("Install a Nerd Font and re-run `niu setup` to unlock them.")
                );
            }
        }
    }

    // --- Preset ---
    let presets = load_presets();
    let mut preset_labels: Vec<String> = presets
        .iter()
        .map(|p| format!("{} {}", pad_display(&p.name, 11), t.tr(&p.summary)))
        .collect();
    preset_labels.push(format!(
        "{} {}",
        pad_display("custom", 11),
        t.tr("answer every question yourself")
    ));
    let preset_refs: Vec<&str> = preset_labels.iter().map(String::as_str).collect();
    let preset_preview = |i: usize| -> Vec<String> {
        match presets.get(i) {
            Some(p) => {
                let mut lines: Vec<String> = theme_preview_line(&p.theme, &p.prompt_symbol)
                    .lines()
                    .map(String::from)
                    .collect();
                lines.push(format!("{} {}", t.tr("packs:"), p.packs.join(" ")));
                lines
            }
            None => vec![t.tr("asks every question one by one").to_string()],
        }
    };
    let preset_idx = ask!(io.choice_preview(
        t.tr("  \u{1f6e0}\u{fe0f}  Choose a setup preset"),
        0,
        &preset_refs,
        t.tr("  \u{2502}  a preset applies a curated configuration in one step; preview follows the highlight"),
        &preset_preview,
    ));

    let mut cfg = if preset_idx == presets.len() {
        ask!(custom_flow(&mut io, nf_capable))
    } else {
        let preset = &presets[preset_idx];
        let mut notes = Vec::new();
        let cfg = preset.to_config(&probe, nf_capable, &mut notes, lang);
        if !notes.is_empty() {
            println!();
            for note in &notes {
                println!("  \u{2502}  {}", note);
            }
        }
        cfg
    };

    // Starship stays opt-in. Segment mode keeps the niubash prompt and lets
    // starship render just {git}; full mode hands the whole prompt to
    // `starship init bash` (bundle plugin). A missing binary is installed via
    // wpm in the tools step below. Presets that don't opt into starship skip
    // the question entirely — `recommended`/`minimal` keep the built-in
    // engine so a preset pick really is one step; only `poweruser` (whose
    // default is the starship segment) and `custom` are asked.
    let ask_git_engine = preset_idx == presets.len()
        || presets
            .get(preset_idx)
            .is_some_and(|preset| preset.starship_default);
    if ask_git_engine && cfg.git_enabled && cfg.prompt_enabled {
        let note = if probe.on_path("starship") {
            t.tr("  \u{2502}  starship detected on PATH")
        } else if wpm_available() {
            t.tr("  \u{2502}  starship will be installed via wpm if selected")
        } else {
            t.tr("  \u{2502}  starship not found and wpm is unavailable")
        };
        let default_idx = match cfg.git_backend {
            GitBackend::Native => 0,
            GitBackend::StarshipSegment => 1,
            GitBackend::StarshipFull => 2,
        };
        let git_engine_options = [
            format!(
                "{}  {}",
                pad_display("Built-in", 16),
                t.tr("native git status inside the niubash prompt")
            ),
            format!(
                "{}  {}",
                pad_display("Starship segment", 16),
                t.tr("starship renders just the {git} part")
            ),
            format!(
                "{}  {}",
                pad_display("Full Starship", 16),
                t.tr("starship owns the whole prompt (replaces theme)")
            ),
        ];
        let git_engine_refs: Vec<&str> = git_engine_options.iter().map(String::as_str).collect();
        let idx = ask!(io.choice(
            t.tr("  \u{1f680}  Git prompt engine"),
            default_idx,
            &git_engine_refs,
            note,
        ));
        cfg.git_backend = [
            GitBackend::Native,
            GitBackend::StarshipSegment,
            GitBackend::StarshipFull,
        ][idx];
    }
    cfg.plugins.retain(|p| p != "starship");
    if cfg.git_backend == GitBackend::StarshipFull {
        cfg.plugins.push("starship".to_string());
    }

    // --- Companion tools via a wpm restore manifest ---
    // Missing tools are offered as cumulative bundles (Essentials / Modern /
    // Everything). Starship joins the manifest whenever a starship backend
    // was selected without the binary on PATH. A reconfigure only offers
    // starship, not the companion bundles.
    // Entries are (binary on PATH, wpm package) pairs — the manifest holds
    // package names while detection and verification use binary names.
    let mut missing: Vec<(String, String)> = Vec::new();
    if cfg.git_backend != GitBackend::Native && !probe.on_path("starship") {
        missing.push(("starship".to_string(), "starship".to_string()));
    }
    let mut installed_tools: Vec<String> = Vec::new();
    if wpm_available() {
        if !reconfigure {
            let tier_missing: Vec<Vec<(String, String)>> = TOOL_TIERS
                .iter()
                .map(|tier| {
                    tier.iter()
                        .filter(|(bin, _)| !probe.on_path(bin))
                        .map(|(bin, pkg)| (bin.to_string(), pkg.to_string()))
                        .collect()
                })
                .collect();
            if tier_missing.iter().any(|tm| !tm.is_empty()) || !missing.is_empty() {
                let mut options: Vec<String> = Vec::new();
                let mut cumulative: Vec<(String, String)> = missing.clone();
                let tier_names = [t.tr("Essentials"), t.tr("Modern CLI"), t.tr("Everything")];
                let mut option_tiers: Vec<usize> = Vec::new();
                for (i, tier) in tier_missing.iter().enumerate() {
                    if tier.is_empty() {
                        continue;
                    }
                    cumulative.extend(tier.iter().cloned());
                    let bins: Vec<&str> = cumulative.iter().map(|(bin, _)| bin.as_str()).collect();
                    options.push(format!(
                        "{}  {}",
                        pad_display(tier_names[i], 12),
                        bins.join(" ")
                    ));
                    option_tiers.push(i);
                }
                options.push(t.tr("Skip").to_string());
                let option_refs: Vec<&str> = options.iter().map(String::as_str).collect();
                let tools_preview = |i: usize| -> Vec<String> {
                    let Some(&tier) = option_tiers.get(i) else {
                        return vec![t.tr("nothing extra installed").to_string()];
                    };
                    let all: Vec<&str> = missing
                        .iter()
                        .chain(tier_missing[..=tier].iter().flatten())
                        .map(|(bin, _)| bin.as_str())
                        .collect();
                    vec![format!("wpm restore: {}", all.join(" "))]
                };
                let pick = ask!(io.choice_preview(
                    t.tr("  \u{1f4e6}  Install companion tools? (optional \u{2014} Niubash itself needs none)"),
                    0,
                    &option_refs,
                    t.tr("  \u{2502}  optional quality-of-life CLI upgrades, installed via wpm\n  \u{2502}  Skip is fine \u{2014} everything works without them; bundles are cumulative\n  \u{2502}  the manifest stays at ~/.niubash/setup-tools.txt"),
                    &tools_preview,
                ));
                if let Some(tier) = option_tiers.get(pick) {
                    let selected: Vec<(String, String)> = missing
                        .iter()
                        .cloned()
                        .chain(tier_missing[..=*tier].iter().flatten().cloned())
                        .collect();
                    installed_tools = install_tools_via_manifest(&selected, &home, lang);
                } else if !missing.is_empty() {
                    println!(
                        "  \u{2502}  {}",
                        t.tr(
                            "skipped \u{2014} starship will not work until `wpm install starship`"
                        )
                    );
                }
            }
        } else if !missing.is_empty() {
            let label = t.tr("  \u{1f4e6}  Install starship via wpm").to_string();
            if ask!(io.yn(&label, true)) {
                installed_tools = install_tools_via_manifest(&missing, &home, lang);
            }
        }
    }

    // --- Windows Terminal profile ---
    let mut want_wt_profile = false;
    if probe.windows_terminal {
        want_wt_profile = ask!(io.yn(
            t.tr("  \u{1f5a5}\u{fe0f}  Register a Niubash profile in Windows Terminal"),
            true
        ));
    }

    // --- Summary + confirm ---
    print_config_summary(&cfg, &installed_tools, want_wt_profile, lang);
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

    if want_wt_profile {
        register_wt_profile(installed_font.as_deref());
    }

    println!();
    if let Some(path) = backup_path {
        println!(
            "  \u{1f4e6}  {}",
            fill(t.tr("Previous rc backed up to {}"), &[&path.display()])
        );
    }
    if !installed_tools.is_empty() {
        println!(
            "  \u{1f4e6}  {}",
            format!(
                "{} {}",
                t.tr("Installed tools:"),
                installed_tools.join(", ")
            )
        );
    }
    println!();
    println!(
        "  \u{1f680}  {}",
        t.tr("You can tweak these settings any time by editing that file.")
    );
    println!(
        "  \u{1f501}  {}",
        t.tr("Run `niu setup` any time to repeat this guide.")
    );
    println!(
        "  \u{1f4a1}  {}",
        t.tr("Full configuration reference: \
             https://github.com/unixwin/niubash/blob/master/docs/src/getting-started.md")
    );
    println!();

    Ok(())
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
    let cfg = preset.to_config(&probe, probe.nerd_font, &mut notes, lang);
    for note in &notes {
        println!("  \u{2502}  {}", note);
    }
    let backup_path = write_rc_and_mark_done(&home, &cfg, lang)?;
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

/// The granular question path: every option asked one at a time.
fn custom_flow(io: &mut WizardIo, nf_capable: bool) -> Option<WizardConfig> {
    let t = io.lang;
    let mut cfg = WizardConfig {
        prompt_style: "off".into(),
        right_prompt: "off".into(),
        symbol: ">".into(),
        completion_style: "ide".into(),
        ..WizardConfig::default()
    };

    cfg.prompt_enabled = io.yn(
        t.tr("  \u{1f3b5}  Enable bundled prompt/theme plugins"),
        true,
    )?;

    let path_preview = |i: usize| -> Vec<String> {
        let dir = ["~/repo/niubash", "C:/Users/name/repo/niubash", "niubash"][i];
        vec![format!("{dir} \u{276f}")]
    };
    cfg.cwd_style = ["home", "full", "basename"][io.choice_preview(
        t.tr("  \u{1f4c1}  Prompt path display"),
        0,
        &["home", "full", "basename"],
        t.tr("  \u{2502}  home     = ~ and ~/repo below your profile\n  \u{2502}  full     = C:/Users/name/repo\n  \u{2502}  basename = only the current directory name"),
        &path_preview,
    )?]
    .to_string();

    if cfg.prompt_enabled {
        let theme_list = theme::list_available_names();
        if theme_list.is_empty() {
            println!(
                "  \u{26a0}\u{fe0f}  {}",
                t.tr("No oh-my-niu themes found \u{2014} skipping theme questions.")
            );
            println!(
                "  \u{2502}  {}",
                t.tr("The bundle should be preinstalled; run `niu setup` again once it is available.")
            );
            cfg.prompt_enabled = false;
        } else {
            let theme_refs: Vec<&str> = theme_list.iter().map(String::as_str).collect();
            let default_idx = theme_refs
                .iter()
                .position(|t| *t == "p10-classic")
                .or_else(|| theme_refs.iter().position(|t| *t == "minimal"))
                .unwrap_or(0);
            let theme_help = if nf_capable {
                t.tr("  \u{2502}  Official themes come from oh-my-niu. Preview follows the highlight.")
            } else {
                t.tr(
                    "  \u{2502}  Themes marked [Nerd Font] need a patched font or they show boxes.",
                )
            };
            let preview = |i: usize| {
                theme_preview_line(theme_refs[i], "\u{276f}")
                    .lines()
                    .map(String::from)
                    .collect()
            };
            cfg.theme = theme_refs[io.choice_preview(
                t.tr("  \u{1f3a8}  Colour theme"),
                default_idx,
                &theme_refs,
                theme_help,
                &preview,
            )?]
            .to_string();

            const SYMBOLS: [&str; 5] = ["\u{276f}", "\u{3bb}", "\u{25b6}", "$", "%"];
            let symbol_preview = |i: usize| -> Vec<String> {
                theme_preview_line(&cfg.theme, SYMBOLS[i])
                    .lines()
                    .map(String::from)
                    .collect()
            };
            cfg.symbol = SYMBOLS[io.choice_preview(
                t.tr("  \u{1f3b5}  Prompt symbol"),
                0,
                &SYMBOLS,
                t.tr("  \u{2502}  \u{276f} heavy right-pointing angle (powerlevel10k style)\n  \u{2502}  \u{3bb} lambda (functional/minimal)\n  \u{2502}  \u{25b6} black right-pointing triangle\n  \u{2502}  $ dollar sign (classic bash)\n  \u{2502}  % percent sign (classic fish)"),
                &symbol_preview,
            )?]
            .to_string();

            let style_preview = |i: usize| -> Vec<String> {
                let t = theme::by_name(&cfg.theme);
                let dir = t.prompt_dir.paint("~/repo").to_string();
                let git = t.git_dirty.paint("main \u{271a} ?").to_string();
                let sym = t.prompt_symbol.paint(&cfg.symbol).to_string();
                match i {
                    0 => vec![format!("{dir} {sym}")],
                    1 => vec![format!("user@host {dir} {git} {sym}")],
                    2 => vec![format!("{dir} {git} {sym}      14:23")],
                    3 => vec![format!("user@host {dir}"), format!("{git} {sym}")],
                    _ => vec![format!("{dir} {git} {sym}      14:23  main *")],
                }
            };
            cfg.prompt_style = ["minimal", "classic", "powerline", "multiline", "segments"]
                [io.choice_preview(
                    t.tr("  \u{1f3b5}  Prompt style"),
                    0,
                    &["minimal", "classic", "powerline", "multiline", "segments"],
                    t.tr("  \u{2502}  minimal   = cwd git prompt_char\n  \u{2502}  classic   = user@host cwd git prompt_char\n  \u{2502}  powerline = compact left prompt with right-side info\n  \u{2502}  multiline = first line context, second line cwd/git\n  \u{2502}  segments  = powerlevel10k-style segment-based prompt"),
                    &style_preview,
                )?]
                .to_string();

            cfg.segment_preset = if cfg.prompt_style == "segments" {
                let seg_preview = |i: usize| -> Vec<String> {
                    let t = theme::by_name(&cfg.theme);
                    let dir = t.prompt_dir.paint("~/repo").to_string();
                    let git = t.git_dirty.paint("main").to_string();
                    let sym = t.prompt_symbol.paint(&cfg.symbol).to_string();
                    match i {
                        0 => vec![format!("{dir} {git} {sym}"), "  icons + separators".into()],
                        1 => vec![format!("{dir} {git} {sym}")],
                        2 => vec![format!("{dir} {git} {sym}"), "  coloured blocks".into()],
                        3 => vec![dir, sym],
                        _ => vec![format!("{sym} {dir} ({git})")],
                    }
                };
                Some(
                    ["classic", "lean", "rainbow", "pure", "robbyrussell"][io.choice_preview(
                        t.tr("  \u{1f3a8}  Segment preset"),
                        0,
                        &["classic", "lean", "rainbow", "pure", "robbyrussell"],
                        t.tr("  \u{2502}  classic       = P10K classic layout\n  \u{2502}  lean          = P10K lean layout\n  \u{2502}  rainbow       = P10K rainbow colours\n  \u{2502}  pure          = P10K pure layout\n  \u{2502}  robbyrussell  = compact classic prompt feel"),
                        &seg_preview,
                    )?]
                    .to_string(),
                )
            } else {
                None
            };

            let right_preview = |i: usize| -> Vec<String> {
                let t = theme::by_name(&cfg.theme);
                let dir = t.prompt_dir.paint("~/repo").to_string();
                let git = t.git_dirty.paint("main *").to_string();
                let sym = t.prompt_symbol.paint(&cfg.symbol).to_string();
                match i {
                    0 => vec![format!("{dir} {git} {sym}")],
                    1 => vec![format!("{dir} {git} {sym}                      14:23")],
                    _ => vec![format!("{dir} {git} {sym}             14:23  main *")],
                }
            };
            cfg.right_prompt = ["off", "time", "full"][io.choice_preview(
                t.tr("  \u{23f1}\u{fe0f}  Right-side info"),
                1,
                &["off", "time", "full"],
                t.tr("  \u{2502}  off  = no right prompt\n  \u{2502}  time = show current time (HH:MM)\n  \u{2502}  full = time + git branch"),
                &right_preview,
            )?]
            .to_string();
        }
    }

    cfg.git_enabled = if cfg.prompt_enabled {
        let git_preview = |i: usize| -> Vec<String> {
            let t = theme::by_name(&cfg.theme);
            let dir = t.prompt_dir.paint("~/repo").to_string();
            let sym = t.prompt_symbol.paint(&cfg.symbol).to_string();
            if i == 0 {
                let git = t.git_dirty.paint("main *").to_string();
                vec![format!("{dir} {git} {sym}")]
            } else {
                vec![format!("{dir} {sym}")]
            }
        };
        let yes_no = [t.tr("Yes"), t.tr("No")];
        io.choice_preview(
            t.tr("  \u{1f500}  Show git branch/status in the prompt"),
            0,
            &yes_no,
            "",
            &git_preview,
        )? == 0
    } else {
        io.yn(t.tr("  \u{1f500}  Load Git helper aliases/functions"), true)?
    };

    let completion_preview = |i: usize| -> Vec<String> {
        match i {
            0 => vec![
                "LICENSE     README.md   build.rs   \u{25b8} build script".into(),
                "src/        target/     Cargo.toml".into(),
            ],
            1 => vec![
                "LICENSE     README.md   build.rs".into(),
                "src/        target/     Cargo.toml".into(),
            ],
            2 => vec![
                "LICENSE     license file".into(),
                "README.md   project readme".into(),
            ],
            _ => vec!["$ cat Car\u{21e5}  \u{2192}  Cargo.toml".into()],
        }
    };
    cfg.completion_style = ["ide", "column", "list", "inline"][io.choice_preview(
        t.tr("  \u{1f5b1}\u{fe0f}  Tab completion style"),
        0,
        &["ide", "column", "list", "inline"],
        t.tr("  \u{2502}  ide    = multi-column popup with descriptions (VS Code style, default)\n  \u{2502}  column = multi-column grid (zsh style)\n  \u{2502}  list   = vertical list with descriptions (fish style)\n  \u{2502}  inline = insert first match, Tab cycles (bash menu-complete)"),
        &completion_preview,
    )?]
    .to_string();

    cfg.plugins = plugin_list(cfg.prompt_enabled, cfg.git_enabled);
    Some(cfg)
}

/// Render the final confirmation table before anything is written.
fn print_config_summary(cfg: &WizardConfig, tools: &[String], wt_profile: bool, lang: Lang) {
    let t = lang;
    let on_off = |b: bool| if b { t.tr("on") } else { t.tr("off") };
    let row = |en: &str, value: String| {
        println!("  \u{2502}  {} {}", pad_display(t.tr(en), 13), value);
    };
    println!();
    println!("  \u{1f4cb}  {}", t.tr("Summary"));
    row(
        "theme",
        if cfg.theme.is_empty() {
            t.tr("(none)").to_string()
        } else {
            cfg.theme.clone()
        },
    );
    row(
        "prompt",
        format!(
            "{} '{}'  {}: {}",
            cfg.prompt_style,
            cfg.symbol,
            t.tr("right"),
            cfg.right_prompt
        ),
    );
    row("cwd style", cfg.cwd_style.clone());
    row("git", on_off(cfg.git_enabled).to_string());
    row(
        "starship",
        match cfg.git_backend {
            GitBackend::Native => t.tr("off").to_string(),
            GitBackend::StarshipSegment => t.tr("git segment").to_string(),
            GitBackend::StarshipFull => t.tr("full prompt").to_string(),
        },
    );
    row("completion", cfg.completion_style.clone());
    row("plugins", cfg.plugins.join(" "));
    if !cfg.aliases.is_empty() {
        row(
            "aliases",
            format!("{} {}", cfg.aliases.len(), t.tr("custom")),
        );
    }
    if !tools.is_empty() {
        row(
            "tools",
            format!("{} {}", t.tr("installing:"), tools.join(" ")),
        );
    }
    if wt_profile {
        row("terminal", t.tr("+ Windows Terminal profile").to_string());
    }
    println!();
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

    println!();
    println!(
        "  \u{2705}  {}",
        fill(lang.tr("Shell rc written to {}"), &[&rc_path.display()])
    );
    Ok(backup_path)
}

#[cfg(windows)]
fn register_wt_profile(font_face: Option<&str>) {
    let lang = Lang::detect();
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let icon = wt_icon_path(&exe);
    match crate::windows_terminal::install_niubash_profile(&exe, icon.as_deref(), false, font_face)
    {
        Ok(summary) if !summary.updated.is_empty() => {
            println!(
                "  \u{1f5a5}\u{fe0f}  {}",
                lang.tr("Windows Terminal profile registered.")
            );
        }
        _ => println!(
            "  \u{26a0}\u{fe0f}  {}",
            lang.tr("Could not register the Windows Terminal profile.")
        ),
    }
}

/// Windows Terminal only exists on Windows; the wizard only reaches this
/// through `WT_SESSION`, which is never set elsewhere.
#[cfg(not(windows))]
fn register_wt_profile(_font_face: Option<&str>) {}

/// Point the Windows Terminal Niubash profile at `face` (best-effort).
#[cfg(windows)]
fn try_set_wt_profile_font(face: &str) {
    let _ = crate::windows_terminal::set_niubash_profile_font(face);
}

#[cfg(not(windows))]
fn try_set_wt_profile_font(_face: &str) {}

#[cfg(windows)]
fn wt_icon_path(commandline: &std::path::Path) -> Option<PathBuf> {
    let app_dir = commandline.parent()?;
    [
        app_dir.join("assets").join("niubash-icon-256.png"),
        app_dir.join("assets").join("niubash-icon.png"),
        app_dir.join("niubash-icon-256.png"),
        app_dir.join("niubash-icon.png"),
    ]
    .into_iter()
    .find(|path| path.is_file())
}

// ── Helpers ──────────────────────────────────────────────────────────────────

/// `wpm` when the command link is on PATH, else `winuxcmd.exe wpm`.
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

fn wpm_available() -> bool {
    wpm_command().is_some()
}

/// Write the wpm package list to `~/.niubash/setup-tools.txt` and install
/// everything in one shot via `wpm restore`. `tools` are `(binary, package)`
/// pairs; the manifest lists packages, and the return value is the binaries
/// verified present after the restore. The manifest stays on disk so the
/// setup is reproducible.
fn install_tools_via_manifest(
    tools: &[(String, String)],
    home: &std::path::Path,
    lang: Lang,
) -> Vec<String> {
    let Some(mut wpm) = wpm_command() else {
        return Vec::new();
    };
    let niubash_dir = home.join(".niubash");
    let _ = std::fs::create_dir_all(&niubash_dir);
    let manifest = niubash_dir.join("setup-tools.txt");
    let mut body = String::from(
        "# Niubash setup companion tools.\n# Reinstall with: wpm restore ~/.niubash/setup-tools.txt\n",
    );
    for (_, package) in tools {
        body.push_str(package);
        body.push('\n');
    }
    if let Err(err) = std::fs::write(&manifest, &body) {
        println!(
            "  \u{26a0}\u{fe0f}  {}",
            format!(
                "{} {} {err}",
                lang.tr("could not write"),
                manifest.display()
            )
        );
        return Vec::new();
    }
    println!();
    println!("  \u{1f4e6}  wpm restore {}", manifest.display());
    let run_restore = |wpm: &mut std::process::Command, force: bool| {
        let mut cmd = wpm.arg("restore");
        if force {
            cmd = cmd.arg("--force");
        }
        cmd.arg(&manifest).stdin(Stdio::null()).status()
    };
    let ok = matches!(run_restore(&mut wpm, false), Ok(s) if s.success())
        || matches!(run_restore(&mut wpm, true), Ok(s) if s.success());
    if !ok {
        println!(
            "  \u{26a0}\u{fe0f}  {}",
            lang.tr("wpm restore failed (see output above)")
        );
    }
    println!();
    tools
        .iter()
        .filter(|(bin, _)| tool_installed(bin))
        .map(|(bin, _)| bin.clone())
        .collect()
}

/// Post-install verification: on PATH, or next to the winuxcmd executable /
/// its `usr/bin` links dir.
fn tool_installed(tool: &str) -> bool {
    if on_path(tool) {
        return true;
    }
    let Some(exe) = crate::winuxcmd::find_winuxcmd() else {
        return false;
    };
    let Some(dir) = exe.parent() else {
        return false;
    };
    [dir.to_path_buf(), dir.join("usr").join("bin")]
        .iter()
        .any(|d| d.join(format!("{tool}.exe")).is_file())
}

// ── Presets ──────────────────────────────────────────────────────────────────

impl Preset {
    /// Look up a built-in preset by name; panics only on a programmer error.
    fn builtin(name: &str) -> Preset {
        builtin_presets()
            .into_iter()
            .find(|p| p.name == name)
            .expect("built-in preset exists")
    }

    /// Expand this preset into a `WizardConfig` for the probed environment.
    /// Human-readable notes about skipped packs/fonts go to `notes`.
    fn to_config(
        &self,
        probe: &EnvProbe,
        nf_capable: bool,
        notes: &mut Vec<String>,
        lang: Lang,
    ) -> WizardConfig {
        let available = available_pack_names();
        let mut plugins: Vec<String> = Vec::new();
        for name in &self.packs {
            push_pack(&mut plugins, name, available.as_ref(), notes, lang);
        }
        for (pack, bin) in &self.conditional_packs {
            if probe.on_path(bin) {
                push_pack(&mut plugins, pack, available.as_ref(), notes, lang);
            } else {
                notes.push(fill(
                    lang.tr("pack '{}' skipped ('{}' not found on PATH)"),
                    &[pack, bin],
                ));
            }
        }

        let mut theme = self.theme.clone();
        let mut symbol = self.prompt_symbol.clone();
        if self.requires_nerd_font && !nf_capable {
            notes.push(fill(
                lang.tr("theme '{}' needs a Nerd Font \u{2014} using 'classic'"),
                &[&self.theme],
            ));
            theme = "classic".into();
            symbol = ">".into();
        }

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
            theme,
            symbol,
            prompt_style: self.prompt_style.clone(),
            right_prompt: self.right_prompt.clone(),
            cwd_style: self.cwd_style.clone(),
            prompt_enabled: true,
            git_enabled: plugins.iter().any(|p| p == "git"),
            // wpm can fetch starship during the tools step, so the preset
            // defaults to the starship segment even when the binary is not
            // installed yet.
            git_backend: if self.starship_default
                && (probe.on_path("starship") || probe.on_path("wpm"))
            {
                GitBackend::StarshipSegment
            } else {
                GitBackend::Native
            },
            segment_preset: None,
            completion_style: self.completion_style.clone(),
            plugins,
            aliases,
        }
    }
}

fn push_pack(
    plugins: &mut Vec<String>,
    name: &str,
    available: Option<&BTreeSet<String>>,
    notes: &mut Vec<String>,
    lang: Lang,
) {
    if let Some(set) = available {
        if !set.contains(name) {
            notes.push(fill(
                lang.tr("pack '{}' not in the bundle \u{2014} skipped"),
                &[&name],
            ));
            return;
        }
    }
    if !plugins.iter().any(|p| p == name) {
        plugins.push(name.to_string());
    }
}

/// The three presets compiled into niubash. Bundle `presets/*.toml` files may
/// extend or override them; keep these safe on a bare install.
fn builtin_presets() -> Vec<Preset> {
    let recommended_aliases: BTreeMap<String, String> = [
        ("ll", "ls -la"),
        ("la", "ls -a"),
        ("l", "ls -F"),
        ("..", "cd .."),
        ("...", "cd ../.."),
        ("cls", "clear"),
        ("apt", "wpm"),
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

    vec![
        Preset {
            name: "recommended".into(),
            summary: "curated daily driver: spaceship theme, git, smart aliases".into(),
            requires_nerd_font: true,
            theme: "spaceship".into(),
            prompt_symbol: "\u{276f}".into(),
            prompt_style: "minimal".into(),
            right_prompt: "time".into(),
            cwd_style: "home".into(),
            completion_style: "column".into(),
            packs: [
                "prompt-core",
                "prompts",
                "themes",
                "git",
                "keybindings",
                "common-aliases",
                "command-not-found",
                "last-working-dir",
                "dotenv",
            ]
            .iter()
            .map(|s| s.to_string())
            .collect(),
            aliases: recommended_aliases.clone(),
            conditional_packs: [("fzf", "fzf"), ("zoxide", "zoxide")]
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            conditional_aliases: recommended_cond_aliases.clone(),
            starship_default: false,
        },
        Preset {
            name: "poweruser".into(),
            summary: "everything above plus starship, direnv, and tool-specific packs".into(),
            requires_nerd_font: true,
            theme: "spaceship".into(),
            prompt_symbol: "\u{276f}".into(),
            prompt_style: "minimal".into(),
            right_prompt: "full".into(),
            cwd_style: "home".into(),
            completion_style: "list".into(),
            packs: [
                "prompt-core",
                "prompts",
                "themes",
                "git",
                "keybindings",
                "common-aliases",
                "command-not-found",
                "last-working-dir",
                "dotenv",
                "extract",
                "path-tools",
                "env-sync",
            ]
            .iter()
            .map(|s| s.to_string())
            .collect(),
            aliases: recommended_aliases,
            conditional_packs: [
                ("fzf", "fzf"),
                ("zoxide", "zoxide"),
                ("direnv", "direnv"),
                ("thefuck", "thefuck"),
                ("kubectl", "kubectl"),
                ("docker", "docker"),
                ("npm", "npm"),
            ]
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
            conditional_aliases: recommended_cond_aliases,
            starship_default: true,
        },
        Preset {
            name: "minimal".into(),
            summary: "safe everywhere: classic theme, git prompt, no extra tooling".into(),
            requires_nerd_font: false,
            theme: "classic".into(),
            prompt_symbol: ">".into(),
            prompt_style: "minimal".into(),
            right_prompt: "off".into(),
            cwd_style: "home".into(),
            completion_style: "column".into(),
            packs: ["prompt-core", "git"]
                .iter()
                .map(|s| s.to_string())
                .collect(),
            aliases: BTreeMap::new(),
            conditional_packs: BTreeMap::new(),
            conditional_aliases: BTreeMap::new(),
            starship_default: false,
        },
    ]
}

/// Built-in presets plus any `presets/*.toml` shipped by the active bundle;
/// bundle presets override built-ins of the same name.
fn load_presets() -> Vec<Preset> {
    let mut presets = builtin_presets();
    let Some(dir) = bundle_presets_dir() else {
        return presets;
    };
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return presets;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("toml") {
            continue;
        }
        let parsed = std::fs::read_to_string(&path)
            .ok()
            .and_then(|text| toml::from_str::<Preset>(&text).ok());
        match parsed {
            Some(preset) if !preset.name.is_empty() => {
                if let Some(existing) = presets.iter_mut().find(|p| p.name == preset.name) {
                    *existing = preset;
                } else {
                    presets.push(preset);
                }
            }
            _ => log::warn!("ignoring unparsable preset {}", path.display()),
        }
    }
    presets
}

/// Candidate bundle roots, mirroring both the registry inventory and the
/// rc-side `NIUBASH` search list so presets work in dev builds too.
fn bundle_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(path) = crate::plugins::active_plugin_inventory().path {
        dirs.push(path);
    }
    if let Some(path) = std::env::var_os("NIU_APP_BUNDLE_PATH") {
        dirs.push(PathBuf::from(path));
    }
    let home = setup_home_dir();
    dirs.push(home.join(".oh-my-niu"));
    dirs.push(home.join(".niubash").join("oh-my-niu"));
    if let Ok(entries) = std::fs::read_dir(home.join(".niubash").join("bundles")) {
        dirs.extend(
            entries.flatten().map(|e| e.path()).filter(|p| {
                p.join("oh-my-niu.winux").is_file() || p.join("oh-my-niu.niu").is_file()
            }),
        );
    }
    dirs
}

fn bundle_presets_dir() -> Option<PathBuf> {
    bundle_dirs()
        .into_iter()
        .map(|d| d.join("presets"))
        .find(|d| d.is_dir())
}

/// Plugin names the oh-my-niu framework loader can resolve:
/// `<bundle>/plugins/<name>/<name>.plugin.niu` plus the user custom dir.
/// `None` when no bundle resolved — preset pack names are trusted as-is.
fn available_pack_names() -> Option<BTreeSet<String>> {
    let mut roots: Vec<PathBuf> = bundle_dirs()
        .into_iter()
        .map(|d| d.join("plugins"))
        .collect();
    roots.push(
        setup_home_dir()
            .join(".niubash")
            .join("custom")
            .join("plugins"),
    );
    if roots.is_empty() {
        return None;
    }
    let mut names = BTreeSet::new();
    let mut found_any = false;
    for root in roots {
        if let Ok(entries) = std::fs::read_dir(&root) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                if entry.path().join(format!("{name}.plugin.niu")).is_file() {
                    names.insert(name);
                    found_any = true;
                }
            }
        }
    }
    found_any.then_some(names)
}

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

fn generate_rc(cfg: &WizardConfig) -> String {
    let theme = if cfg.prompt_enabled {
        cfg.theme.as_str()
    } else {
        ""
    };
    let symbol = if cfg.prompt_enabled {
        cfg.symbol.as_str()
    } else {
        ">"
    };
    let (prompt_template, right_template) = if cfg.prompt_style == "segments" {
        match cfg.segment_preset.as_deref().unwrap_or("classic") {
            "pure" => (
                "{cwd} {git} {command_execution_time}{newline}{prompt_char} ".to_string(),
                String::new(),
            ),
            "robbyrussell" => (
                "{cwd} {git}{newline}{prompt_char} ".to_string(),
                String::new(),
            ),
            "lean" => (
                "{cwd} {git}{newline}{prompt_char} ".to_string(),
                String::new(),
            ),
            "rainbow" | "classic" => (
                "{cwd} {git}{newline}{prompt_char} ".to_string(),
                "{status}{time} ".to_string(),
            ),
            _ => (
                "{cwd} {git}{newline}{prompt_char} ".to_string(),
                "{status}{time} ".to_string(),
            ),
        }
    } else {
        match (cfg.prompt_style.as_str(), cfg.right_prompt.as_str()) {
            ("powerline", "time") => ("{cwd} {git} ".to_string(), "{time} ".to_string()),
            ("powerline", "full") => ("{cwd} {git} ".to_string(), "{time} {git} ".to_string()),
            ("powerline", _) => ("{cwd} {git} ".to_string(), String::new()),
            ("multiline", "time") => (
                "{user}@{host} {time}\n{cwd} {git} ".to_string(),
                String::new(),
            ),
            ("multiline", "full") => (
                "{user}@{host} {time}\n{cwd} {git} ".to_string(),
                "{git} ".to_string(),
            ),
            ("multiline", _) => ("{user}@{host}\n{cwd} {git} ".to_string(), String::new()),
            ("classic", "time") => (
                "{user}@{host} {cwd} {git} ".to_string(),
                "{time} ".to_string(),
            ),
            ("classic", "full") => (
                "{user}@{host} {cwd} {git} ".to_string(),
                "{time} {git} ".to_string(),
            ),
            ("classic", _) => ("{user}@{host} {cwd} {git} ".to_string(), String::new()),
            ("minimal", "time") => ("{cwd} ".to_string(), "{time} ".to_string()),
            ("minimal", "full") => ("{cwd} ".to_string(), "{time} {git_branch} ".to_string()),
            _ => ("{cwd} ".to_string(), String::new()),
        }
    };
    let prompt_template = if cfg.git_enabled {
        prompt_template
    } else {
        strip_git_prompt_tokens(&prompt_template)
    };
    let right_template = if cfg.git_enabled {
        right_template
    } else {
        strip_git_prompt_tokens(&right_template)
    };
    let theme_plugin = if cfg.prompt_enabled {
        theme_plugin_name(theme)
    } else {
        String::new()
    };
    let plugins = format!("({})", cfg.plugins.join(" "));
    // Must be exported before the bundle loads: prompt-core reads
    // NIU_PROMPT_GIT_BACKEND when it initializes.
    let starship_segment_setup = if cfg.git_backend == GitBackend::StarshipSegment {
        "NIU_PROMPT_GIT_BACKEND=starship\nexport NIU_PROMPT_GIT_BACKEND\n".to_string()
    } else {
        String::new()
    };
    let segment_note = cfg
        .segment_preset
        .as_deref()
        .map(|preset| format!("# Segment preset selected during setup: {preset}\n"))
        .unwrap_or_default();
    let alias_block = if cfg.aliases.is_empty() {
        String::new()
    } else {
        let mut block = String::from("# Aliases\n");
        for (name, cmd) in &cfg.aliases {
            block.push_str(&format!("alias {}={}\n", name, shell_quote(cmd)));
        }
        block
    };
    let prompt_call = if cfg.git_backend == GitBackend::StarshipFull {
        "# Prompt owned by Starship (the starship plugin runs `starship init bash`).\n".to_string()
    } else if cfg.prompt_enabled {
        format!(
            "niubash_prompt_use_template {} {} 2>/dev/null || true\n",
            shell_quote(&prompt_template),
            shell_quote(&right_template)
        )
    } else {
        "# Prompt/theme plugins disabled by setup.\n".to_string()
    };
    format!(
        r#"# Niubash interactive rc — generated by the setup wizard.
# Edit this file with normal Niubash/bash syntax.
# Structured TOML manifests are not user startup configuration; new interactive setup
# should live here.

NIU_THEME={}
NIU_THEME_PLUGIN={}
NIU_PROMPT_SYMBOL={}
NIU_PROMPT_CWD_STYLE={}
NIU_COMPLETION_STYLE={}
NIU_DISABLE_DEFAULT_PLUGINS=1
export NIU_THEME NIU_THEME_PLUGIN NIU_PROMPT_SYMBOL
export NIU_PROMPT_CWD_STYLE NIU_COMPLETION_STYLE NIU_DISABLE_DEFAULT_PLUGINS

NIU_PLUGINS={}
{}
{}
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

if [ -z "${{NIUBASH:-}}" ]; then
  for __niubash_bundle in "$HOME/.oh-my-niu" "$HOME/.niubash/oh-my-niu" "$HOME/.niubash/bundles/oh-my-niu"/* "$NIU_APP_BUNDLE_PATH"; do
    if [ -f "$__niubash_bundle/oh-my-niu.niu" ] || [ -f "$__niubash_bundle/oh-my-niu.winux" ]; then
      NIUBASH="$__niubash_bundle"
      export NIUBASH
      break
    fi
  done
fi

if [ -f "$NIUBASH/oh-my-niu.niu" ]; then
  . "$NIUBASH/oh-my-niu.niu"
elif [ -f "$NIUBASH/oh-my-niu.winux" ]; then
  . "$NIUBASH/oh-my-niu.winux"
fi

{}
{}
unset __niubash_bundle __niubash_home_drive __niubash_home_rest
"#,
        shell_quote(theme),
        shell_quote(&theme_plugin),
        shell_quote(symbol),
        shell_quote(cfg.cwd_style.as_str()),
        shell_quote(cfg.completion_style.as_str()),
        plugins,
        starship_segment_setup,
        segment_note,
        alias_block,
        prompt_call,
    )
}

/// The plugin list for the custom flow: mirrors the wizard's own decisions,
/// independent of preset packs.
fn plugin_list(prompt_enabled: bool, git_enabled: bool) -> Vec<String> {
    let mut plugins = Vec::new();
    if prompt_enabled {
        plugins.push("prompt-core".to_string());
    }
    if git_enabled {
        plugins.push("git".to_string());
    }
    plugins
}

fn theme_plugin_name(theme: &str) -> String {
    if theme.starts_with("theme-") {
        theme.to_string()
    } else {
        format!("theme-{}", theme)
    }
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', r#"'\''"#))
}

fn strip_git_prompt_tokens(value: &str) -> String {
    value
        .replace("{git_prompt}", "")
        .replace("{git}", "")
        .replace("{git_branch}", "")
        .replace("{git_status}", "")
        .replace("  ", " ")
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

fn theme_preview_line(theme_name: &str, symbol: &str) -> String {
    let theme = theme::by_name(theme_name);
    let dir = theme.prompt_dir.paint("~/repo/niubash").to_string();
    let git = theme.git_dirty.paint("codex/theme-api ✚ ? *").to_string();
    let prompt = theme.prompt_symbol.paint(symbol).to_string();
    let note = if nerd_font_theme(theme_name) {
        " [Nerd Font]"
    } else {
        ""
    };
    format!("{theme_name:<22} {dir} {git}\n  \u{2502}                           {prompt} {note}")
}

fn nerd_font_theme(theme_name: &str) -> bool {
    matches!(
        theme_name,
        "agnoster"
            | "dracula"
            | "catppuccin-mocha"
            | "gruvbox"
            | "spaceship"
            | "tokyonight"
            | "p10-classic"
            | "p10-lean"
            | "p10-rainbow"
            | "p10-pure"
    )
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
        "fail after setup, restart niu or run `wpm links rebuild`." =>
            "无法使用，请重启 niu 或运行 `wpm links rebuild`。",

        // Nerd Font step
        "Glyph sample:" => "字形示例：",
        "  (recommended)" => "  （推荐）",
        "Skip \u{2014} keep my current font" => "跳过 —— 保留当前字体",
        "  \u{1f5a4}\u{fe0f}  Install a Nerd Font for icon-rich themes?" =>
            "  \u{1f5a4}\u{fe0f}  安装 Nerd Font 以启用图标主题？",
        "  \u{2502}  downloads from nerd-fonts and installs per-user \u{2014} no admin needed" =>
            "  \u{2502}  从 nerd-fonts 官方发布下载，按用户安装 —— 无需管理员权限",
        "Installed {} ({} files)" => "已安装 {}（{} 个文件）",
        "Niubash tabs in Windows Terminal will use '{}'." =>
            "Windows Terminal 中的 Niubash 标签页将使用 '{}'。",
        "Now set your terminal font to '{}'." => "请把终端字体设置为 '{}'。",
        "Font install failed:" => "字体安装失败：",
        "  \u{1f524}  Do the glyphs above render correctly (not boxes)?" =>
            "  \u{1f524}  上面的字形显示正常吗（不是方框/乱码）？",
        "Nerd-Font themes fall back to 'classic' for now." =>
            "Nerd Font 主题暂时回退为 'classic'。",
        "Install a Nerd Font and re-run `niu setup` to unlock them." =>
            "安装 Nerd Font 后重新运行 `niu setup` 即可解锁。",

        // Preset step
        "curated daily driver: spaceship theme, git, smart aliases" =>
            "精选日常配置：spaceship 主题、git、智能别名",
        "everything above plus starship, direnv, and tool-specific packs" =>
            "在 recommended 基础上增加 starship、direnv 和工具专属插件包",
        "safe everywhere: classic theme, git prompt, no extra tooling" =>
            "处处可用：classic 主题、git 提示符、不装额外工具",
        "answer every question yourself" => "逐项回答每个问题",
        "  \u{1f6e0}\u{fe0f}  Choose a setup preset" => "  \u{1f6e0}\u{fe0f}  选择一个设置预设",
        "  \u{2502}  a preset applies a curated configuration in one step; preview follows the highlight" =>
            "  \u{2502}  预设一步应用整套配置；预览随高亮选项实时变化",
        "packs:" => "插件包：",
        "asks every question one by one" => "逐项询问每个问题",

        // Starship step
        "  \u{2502}  starship detected on PATH" => "  \u{2502}  已在 PATH 上检测到 starship",
        "  \u{2502}  starship will be installed via wpm if selected" =>
            "  \u{2502}  若选择 starship，将通过 wpm 自动安装",
        "  \u{2502}  starship not found and wpm is unavailable" =>
            "  \u{2502}  未找到 starship 且 wpm 不可用",
        "  \u{1f680}  Git prompt engine" => "  \u{1f680}  Git 提示符引擎",
        "native git status inside the niubash prompt" => "niubash 提示符内置的原生 git 状态",
        "starship renders just the {git} part" => "starship 只渲染 {git} 部分",
        "starship owns the whole prompt (replaces theme)" =>
            "starship 接管整个提示符（替换主题）",

        // Companion tools step
        "Essentials" => "基础工具",
        "Modern CLI" => "现代 CLI",
        "Everything" => "全部安装",
        "Skip" => "跳过",
        "nothing extra installed" => "不额外安装任何工具",
        "  \u{1f4e6}  Install companion tools? (optional \u{2014} Niubash itself needs none)" =>
            "  \u{1f4e6}  安装配套命令行工具？（可选 —— Niubash 本体不依赖它们）",
        "  \u{2502}  optional quality-of-life CLI upgrades, installed via wpm\n  \u{2502}  Skip is fine \u{2014} everything works without them; bundles are cumulative\n  \u{2502}  the manifest stays at ~/.niubash/setup-tools.txt" =>
            "  \u{2502}  可选的效率工具升级，通过 wpm 安装；跳过完全不影响使用\n  \u{2502}  套餐逐级包含；安装清单保存在 ~/.niubash/setup-tools.txt",
        "skipped \u{2014} starship will not work until `wpm install starship`" =>
            "已跳过 —— 运行 `wpm install starship` 之前 starship 不可用",
        "  \u{1f4e6}  Install starship via wpm" => "  \u{1f4e6}  通过 wpm 安装 starship",
        "could not write" => "无法写入",
        "wpm restore failed (see output above)" => "wpm restore 失败（详见上方输出）",

        // Windows Terminal
        "  \u{1f5a5}\u{fe0f}  Register a Niubash profile in Windows Terminal" =>
            "  \u{1f5a5}\u{fe0f}  在 Windows Terminal 中注册 Niubash 配置文件",
        "Windows Terminal profile registered." => "Windows Terminal 配置文件已注册。",
        "Could not register the Windows Terminal profile." =>
            "无法注册 Windows Terminal 配置文件。",

        // Summary (labels shared with the environment summary above)
        "Summary" => "配置摘要",
        "theme" => "主题",
        "prompt" => "提示符",
        "cwd style" => "路径样式",
        "git" => "git",
        "starship" => "starship",
        "completion" => "补全",
        "plugins" => "插件",
        "aliases" => "别名",
        "(none)" => "（无）",
        "right" => "右侧",
        "on" => "开",
        "off" => "关",
        "git segment" => "git 段",
        "full prompt" => "整个提示符",
        "custom" => "条自定义",
        "installing:" => "将安装：",
        "+ Windows Terminal profile" => "+ Windows Terminal 配置文件",

        // Confirm + final messages
        "  \u{2705}  Apply this configuration?" => "  \u{2705}  应用此配置？",
        "Apply" => "应用",
        "Cancel" => "取消",
        "Nothing was written." => "未写入任何内容。",
        "Setup cancelled \u{2014} nothing was written." =>
            "设置已取消 —— 未写入任何内容。",
        "Shell rc written to {}" => "Shell 配置已写入 {}",
        "Previous rc backed up to {}" => "原 rc 已备份至 {}",
        "Installed tools:" => "已安装工具：",
        "You can tweak these settings any time by editing that file." =>
            "随时编辑该文件即可调整这些设置。",
        "Run `niu setup` any time to repeat this guide." =>
            "随时运行 `niu setup` 可重新进入本向导。",
        "Full configuration reference: https://github.com/unixwin/niubash/blob/master/docs/src/getting-started.md" =>
            "完整配置参考：https://github.com/unixwin/niubash/blob/master/docs/src/getting-started.md",

        // apply_preset
        "unknown preset" => "未知预设",
        "available:" => "可用：",
        "Preset applied:" => "预设已应用：",

        // Custom flow
        "  \u{1f3b5}  Enable bundled prompt/theme plugins" =>
            "  \u{1f3b5}  启用内置提示符/主题插件",
        "  \u{1f4c1}  Prompt path display" => "  \u{1f4c1}  提示符路径显示",
        "  \u{2502}  home     = ~ and ~/repo below your profile\n  \u{2502}  full     = C:/Users/name/repo\n  \u{2502}  basename = only the current directory name" =>
            "  \u{2502}  home     = 以 ~ 表示主目录，如 ~/repo\n  \u{2502}  full     = 完整路径 C:/Users/name/repo\n  \u{2502}  basename = 只显示当前目录名",
        "No oh-my-niu themes found \u{2014} skipping theme questions." =>
            "未找到 oh-my-niu 主题 —— 跳过主题问题。",
        "The bundle should be preinstalled; run `niu setup` again once it is available." =>
            "发行包应预置该 bundle；就绪后请重新运行 `niu setup`。",
        "  \u{1f3a8}  Colour theme" => "  \u{1f3a8}  配色主题",
        "  \u{2502}  Official themes come from oh-my-niu. Preview follows the highlight." =>
            "  \u{2502}  官方主题来自 oh-my-niu。预览随高亮实时变化。",
        "  \u{2502}  Themes marked [Nerd Font] need a patched font or they show boxes." =>
            "  \u{2502}  标注 [Nerd Font] 的主题需要专用字体，否则会显示为方框。",
        "  \u{1f3b5}  Prompt symbol" => "  \u{1f3b5}  提示符符号",
        "  \u{2502}  \u{276f} heavy right-pointing angle (powerlevel10k style)\n  \u{2502}  \u{3bb} lambda (functional/minimal)\n  \u{2502}  \u{25b6} black right-pointing triangle\n  \u{2502}  $ dollar sign (classic bash)\n  \u{2502}  % percent sign (classic fish)" =>
            "  \u{2502}  \u{276f} 加重右尖角（powerlevel10k 风格）\n  \u{2502}  \u{3bb} lambda（函数式/极简）\n  \u{2502}  \u{25b6} 黑色右三角\n  \u{2502}  $ 美元符（经典 bash）\n  \u{2502}  % 百分号（经典 fish）",
        "  \u{1f3b5}  Prompt style" => "  \u{1f3b5}  提示符样式",
        "  \u{2502}  minimal   = cwd git prompt_char\n  \u{2502}  classic   = user@host cwd git prompt_char\n  \u{2502}  powerline = compact left prompt with right-side info\n  \u{2502}  multiline = first line context, second line cwd/git\n  \u{2502}  segments  = powerlevel10k-style segment-based prompt" =>
            "  \u{2502}  minimal   = 路径 git 符号\n  \u{2502}  classic   = user@host 路径 git 符号\n  \u{2502}  powerline = 紧凑左提示 + 右侧信息\n  \u{2502}  multiline = 首行上下文，第二行路径/git\n  \u{2502}  segments  = powerlevel10k 式分段提示符",
        "  \u{1f3a8}  Segment preset" => "  \u{1f3a8}  分段预设",
        "  \u{2502}  classic       = P10K classic layout\n  \u{2502}  lean          = P10K lean layout\n  \u{2502}  rainbow       = P10K rainbow colours\n  \u{2502}  pure          = P10K pure layout\n  \u{2502}  robbyrussell  = compact classic prompt feel" =>
            "  \u{2502}  classic       = P10K classic 布局\n  \u{2502}  lean          = P10K lean 布局\n  \u{2502}  rainbow       = P10K rainbow 配色\n  \u{2502}  pure          = P10K pure 布局\n  \u{2502}  robbyrussell  = 紧凑经典提示符",
        "  \u{23f1}\u{fe0f}  Right-side info" => "  \u{23f1}\u{fe0f}  右侧信息",
        "  \u{2502}  off  = no right prompt\n  \u{2502}  time = show current time (HH:MM)\n  \u{2502}  full = time + git branch" =>
            "  \u{2502}  off  = 无右侧提示\n  \u{2502}  time = 显示当前时间（HH:MM）\n  \u{2502}  full = 时间 + git 分支",
        "  \u{1f500}  Show git branch/status in the prompt" =>
            "  \u{1f500}  在提示符中显示 git 分支/状态",
        "  \u{1f500}  Load Git helper aliases/functions" =>
            "  \u{1f500}  加载 Git 辅助别名/函数",
        "  \u{1f5b1}\u{fe0f}  Tab completion style" => "  \u{1f5b1}\u{fe0f}  Tab 补全样式",
        "  \u{2502}  ide    = multi-column popup with descriptions (VS Code style, default)\n  \u{2502}  column = multi-column grid (zsh style)\n  \u{2502}  list   = vertical list with descriptions (fish style)\n  \u{2502}  inline = insert first match, Tab cycles (bash menu-complete)" =>
            "  \u{2502}  ide    = 多列弹窗 + 描述（VS Code 风格，默认）\n  \u{2502}  column = 多列网格（zsh 风格）\n  \u{2502}  list   = 竖排列表 + 描述（fish 风格）\n  \u{2502}  inline = 插入首个匹配，Tab 循环（bash menu-complete）",
        "Yes" => "是",
        "No" => "否",

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

    #[test]
    fn display_welcome_side_by_side_renders_without_panic() {
        display_welcome_side_by_side(false, Lang::En);
        display_welcome_side_by_side(true, Lang::Zh);
    }

    #[test]
    fn setup_home_dir_accepts_shell_style_home_env() {
        let _process_lock = PROCESS_STATE_LOCK.lock().unwrap();
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

    #[allow(clippy::too_many_arguments)]
    fn test_cfg(
        theme: &str,
        style: &str,
        right: &str,
        symbol: &str,
        cwd: &str,
        prompt_enabled: bool,
        git: bool,
        git_backend: GitBackend,
        segment: Option<&str>,
        completion: &str,
    ) -> WizardConfig {
        let mut plugins = plugin_list(prompt_enabled, git);
        if git_backend == GitBackend::StarshipFull {
            plugins.push("starship".to_string());
        }
        WizardConfig {
            theme: theme.to_string(),
            prompt_style: style.to_string(),
            right_prompt: right.to_string(),
            symbol: symbol.to_string(),
            cwd_style: cwd.to_string(),
            prompt_enabled,
            git_enabled: git,
            git_backend,
            segment_preset: segment.map(String::from),
            completion_style: completion.to_string(),
            plugins,
            aliases: Vec::new(),
        }
    }

    #[test]
    fn generated_rc_uses_shell_entrypoint_not_toml_sections() {
        let rc = generate_rc(&test_cfg(
            "minimal",
            "minimal",
            "time",
            ">",
            "home",
            true,
            true,
            GitBackend::Native,
            None,
            "column",
        ));

        assert!(rc.contains("NIU_THEME_PLUGIN='theme-minimal'"));
        assert!(rc.contains("NIU_PROMPT_CWD_STYLE='home'"));
        assert!(rc.contains("NIU_DISABLE_DEFAULT_PLUGINS=1"));
        assert!(rc.contains("NIU_PLUGINS=(prompt-core git)"));
        assert!(rc.contains("\"$NIU_APP_BUNDLE_PATH\""));
        assert!(rc.contains(". \"$NIUBASH/oh-my-niu.niu\""));
        assert!(rc.contains("niubash_prompt_use_template '{cwd} ' '{time} '"));
        assert!(!rc.contains("[plugins]"));
        assert!(!rc.contains("[shell]"));
        assert!(!rc.contains("prompt_format ="));
    }

    #[test]
    fn generated_rc_can_disable_git_prompt_tokens() {
        let rc = generate_rc(&test_cfg(
            "minimal",
            "classic",
            "full",
            "$",
            "full",
            true,
            false,
            GitBackend::Native,
            None,
            "column",
        ));

        assert!(rc.contains("NIU_THEME_PLUGIN='theme-minimal'"));
        assert!(rc.contains("NIU_PROMPT_CWD_STYLE='full'"));
        assert!(rc.contains("NIU_PLUGINS=(prompt-core)"));
        assert!(!rc.contains(" git "));
        assert!(!rc.contains("{git_prompt}"));
        assert!(!rc.contains("{git_branch}"));
    }

    #[test]
    fn generated_rc_can_disable_prompt_theme_plugins() {
        let rc = generate_rc(&test_cfg(
            "",
            "off",
            "off",
            ">",
            "basename",
            false,
            true,
            GitBackend::Native,
            None,
            "column",
        ));

        assert!(rc.contains("NIU_THEME=''"));
        assert!(rc.contains("NIU_THEME_PLUGIN=''"));
        assert!(rc.contains("NIU_PROMPT_CWD_STYLE='basename'"));
        assert!(rc.contains("NIU_DISABLE_DEFAULT_PLUGINS=1"));
        assert!(rc.contains("NIU_PLUGINS=(git)"));
        assert!(rc.contains("# Prompt/theme plugins disabled by setup."));
        assert!(!rc.contains("niubash_prompt_use_template"));
        assert!(!rc.contains("prompt_format ="));
    }

    #[test]
    fn generated_rc_can_delegate_git_segment_to_starship() {
        let rc = generate_rc(&test_cfg(
            "spaceship",
            "multiline",
            "off",
            "%",
            "home",
            true,
            true,
            GitBackend::StarshipSegment,
            None,
            "column",
        ));

        assert!(rc.contains("NIU_THEME_PLUGIN='theme-spaceship'"));
        assert!(rc.contains("NIU_PLUGINS=(prompt-core git)"));
        // The backend env var must be exported before the bundle loads.
        let backend_pos = rc.find("NIU_PROMPT_GIT_BACKEND=starship").unwrap();
        let bundle_pos = rc.find("oh-my-niu.niu\"").unwrap();
        assert!(backend_pos < bundle_pos);
        assert!(rc.contains("niubash_prompt_use_template"));
        assert!(rc.contains("{git}"));
        assert!(!rc.contains("NIU_STARSHIP_SEGMENTS"));
        assert!(!rc.contains("STARSHIP_CONFIG"));
    }

    #[test]
    fn generated_rc_full_starship_owns_the_prompt() {
        let rc = generate_rc(&test_cfg(
            "spaceship",
            "multiline",
            "off",
            "%",
            "home",
            true,
            true,
            GitBackend::StarshipFull,
            None,
            "column",
        ));

        assert!(rc.contains("NIU_PLUGINS=(prompt-core git starship)"));
        assert!(rc.contains("# Prompt owned by Starship"));
        assert!(!rc.contains("niubash_prompt_use_template"));
        assert!(!rc.contains("NIU_PROMPT_GIT_BACKEND"));
    }

    #[test]
    fn generated_rc_includes_completion_style() {
        let rc = generate_rc(&test_cfg(
            "minimal",
            "minimal",
            "off",
            ">",
            "home",
            true,
            true,
            GitBackend::Native,
            None,
            "list",
        ));
        assert!(rc.contains("NIU_COMPLETION_STYLE='list'"));
        assert!(rc.contains("NIU_COMPLETION_STYLE"));

        let rc = generate_rc(&test_cfg(
            "minimal",
            "minimal",
            "off",
            ">",
            "home",
            true,
            true,
            GitBackend::Native,
            None,
            "inline",
        ));
        assert!(rc.contains("NIU_COMPLETION_STYLE='inline'"));
    }

    #[test]
    fn generated_rc_writes_preset_aliases() {
        let mut cfg = test_cfg(
            "minimal",
            "minimal",
            "off",
            ">",
            "home",
            true,
            true,
            GitBackend::Native,
            None,
            "column",
        );
        cfg.aliases = vec![("ll".to_string(), "ls -la".to_string())];
        let rc = generate_rc(&cfg);
        assert!(rc.contains("alias ll='ls -la'"));
    }

    #[test]
    fn builtin_presets_expand_to_configs() {
        let probe = EnvProbe {
            windows_terminal: false,
            mintty_hint: false,
            nerd_font: false,
            command_links: true,
            tools: BTreeSet::new(),
        };
        let presets = builtin_presets();
        assert!(presets.len() >= 3);
        assert!(presets.iter().any(|p| p.name == "recommended"));
        for preset in &presets {
            let mut notes = Vec::new();
            let cfg = preset.to_config(&probe, false, &mut notes, Lang::En);
            assert!(!cfg.completion_style.is_empty());
        }
        // Without a Nerd Font, presets that need one fall back to 'classic'.
        let recommended = presets.iter().find(|p| p.name == "recommended").unwrap();
        let mut notes = Vec::new();
        let cfg = recommended.to_config(&probe, false, &mut notes, Lang::En);
        assert_eq!(cfg.theme, "classic");
        assert!(notes.iter().any(|n| n.contains("Nerd Font")));
        // With a Nerd Font the preset theme survives.
        let cfg = recommended.to_config(&probe, true, &mut Vec::new(), Lang::En);
        assert_eq!(cfg.theme, "spaceship");
    }

    #[test]
    fn zh_translations_cover_the_wizard_vocab() {
        // Every key here is asserted to translate so a renamed English literal
        // fails loudly instead of silently falling back to English.
        for key in [
            "Welcome to Niubash",
            "  \u{1f4e6}  Install companion tools? (optional \u{2014} Niubash itself needs none)",
            "Apply",
            "Cancel",
            "Yes",
            "No",
        ] {
            assert!(zh(key).is_some(), "missing zh translation for {key:?}");
        }
        // Unknown keys fall back to English verbatim.
        assert_eq!(Lang::Zh.tr("untranslated literal"), "untranslated literal");
    }

    #[test]
    fn preset_toml_round_trips() {
        let text = r#"
name = "myteam"
summary = "team preset"
requires_nerd_font = false
theme = "minimal"
packs = ["prompt-core", "git"]

[aliases]
ll = "ls -la"

[conditional_packs]
fzf = "fzf"

[conditional_aliases.eza]
ls = "eza --icons"
"#;
        let preset: Preset = toml::from_str(text).unwrap();
        assert_eq!(preset.name, "myteam");
        assert_eq!(preset.aliases["ll"], "ls -la");
        assert_eq!(preset.conditional_packs["fzf"], "fzf");
        assert_eq!(preset.conditional_aliases["eza"]["ls"], "eza --icons");
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
