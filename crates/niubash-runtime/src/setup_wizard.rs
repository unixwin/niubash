//! First-run setup wizard.
//!
//! Guides the user through initial interactive configuration, then writes a
//! normal shell rc file.

use std::io::{self, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::interactive_menu::{self, Selection};
use crate::path_utils::shell_home_dir;
use crate::theme;

const PRIMARY_RC_FILE: &str = ".niubashrc";
const COMPAT_RC_FILE: &str = ".winuxshrc";
const SETUP_DONE_FILE: &str = ".setup-done";

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
fn display_welcome_side_by_side(reconfigure: bool) {
    let width = crate::interactive_menu::term_width();
    let logo_cols = if width >= 100 { 48 } else { 32 };
    let logo_str = crate::logo::render_logo_to_string(logo_cols);
    let logo_lines: Vec<String> = logo_str.lines().map(String::from).collect();

    let mut content = Vec::new();
    content.push(String::new());
    content.push(format!(
        " {}  Welcome to Niubash {}!",
        "\u{1f389}",
        env!("CARGO_PKG_VERSION")
    ));
    content.push(format!(
        " {}  A bash-compatible shell for Windows \u{2014} no WSL, no MSYS2 required.",
        "\u{2728}"
    ));
    content.push(String::new());
    if reconfigure {
        content.push(
            "  Reconfigure your interactive prompt/plugins. Existing rc will be backed up."
                .to_string(),
        );
    } else {
        content.push("  Let\u{2019}s get you set up.".to_string());
    }
    content.push(String::new());

    crate::interactive_menu::print_side_by_side(&logo_lines, &content, 100);
}

fn run_wizard_inner(reconfigure: bool) -> anyhow::Result<()> {
    let home = setup_home_dir();

    // Show logo + welcome: side-by-side on wide terminals, stacked on narrow
    if !reconfigure {
        display_welcome_side_by_side(false);
    } else {
        display_welcome_side_by_side(true);
    }

    // --- Software preset (first-run only) ---
    let mut preset_packages: Vec<String> = Vec::new();
    if !reconfigure {
        let preset_idx = pick_choice(
            "  \u{1f6e0}\u{fe0f}  Setup preset",
            0,
            &["minimal", "dev", "full"],
            "  \u{2502}  minimal = prompt + git only\n  \u{2502}  dev     = + ripgrep, fd, fzf, bat, starship\n  \u{2502}  full    = + lua, python, node",
        );
        preset_packages = match preset_idx.as_str() {
            "dev" => vec![
                "starship".into(),
                "ripgrep".into(),
                "fd".into(),
                "fzf".into(),
                "bat".into(),
            ],
            "full" => vec![
                "starship".into(),
                "ripgrep".into(),
                "fd".into(),
                "fzf".into(),
                "bat".into(),
                "lua".into(),
                "python".into(),
                "node".into(),
            ],
            _ => vec![],
        };
        if !preset_packages.is_empty() {
            println!();
            println!("  \u{1f4e6}  Will install: {}", preset_packages.join(", "));
            let confirm = prompt_yn("  \u{2753}  Proceed with installation", true);
            if confirm {
                install_wpm_packages(&preset_packages);
            } else {
                preset_packages.clear();
            }
        }
    }

    let prompt_enabled = prompt_yn("  \u{1f3b5}  Enable bundled prompt/theme plugins", true);

    let mut theme = String::new();
    let mut prompt_style = "off".to_string();
    let mut right_prompt = "off".to_string();
    let mut symbol = ">".to_string();
    let mut segment_preset: Option<String> = None;
    let cwd_style = pick_choice(
        "  \u{1f4c1}  Prompt path display",
        0,
        &["home", "full", "basename"],
        "  \u{2502}  home     = ~ and ~/repo below your profile\n  \u{2502}  full     = C:/Users/name/repo\n  \u{2502}  basename = only the current directory name",
    );

    if prompt_enabled {
        // --- Theme ---
        let theme_list = theme::list_available_names();
        if theme_list.is_empty() {
            println!(
                "  \u{26a0}\u{fe0f}  No oh-my-niu themes found \u{2014} skipping theme questions."
            );
            println!("  \u{2502}  The bundle should be preinstalled; run `niu setup` again once it is available.");
        } else {
            let theme_refs: Vec<&str> = theme_list.iter().map(String::as_str).collect();
            let default_idx = theme_refs
                .iter()
                .position(|t| *t == "p10-classic")
                .or_else(|| theme_refs.iter().position(|t| *t == "minimal"))
                .unwrap_or(0);
            print_theme_previews(&theme_refs, "\u{276f}");
            theme = pick_choice(
                "  \u{1f3a8}  Colour theme",
                default_idx,
                &theme_refs,
                "  \u{2502}  Choose the plugin-owned prompt colour scheme. Official themes come from oh-my-niu.",
            );

            // --- Prompt symbol ---
            symbol = pick_choice(
                "  \u{1f3b5}  Prompt symbol",
                0,
                &["\u{276f}", "\u{3bb}", "\u{25b6}", "$", "%"],
                "  \u{2502}  \u{276f} heavy right-pointing angle (powerlevel10k style)\n  \u{2502}  \u{3bb} lambda (functional/minimal)\n  \u{2502}  \u{25b6} black right-pointing triangle\n  \u{2502}  $ dollar sign (classic bash)\n  \u{2502}  % percent sign (classic fish)",
            );

            // --- Prompt style ---
            prompt_style = pick_choice(
                "  \u{1f3b5}  Prompt style",
                0,
                &["minimal", "classic", "powerline", "multiline", "segments"],
                "  \u{2502}  minimal   = cwd git prompt_char\n  \u{2502}  classic   = user@host cwd git prompt_char\n  \u{2502}  powerline = compact left prompt with right-side info\n  \u{2502}  multiline = first line context, second line cwd/git\n  \u{2502}  segments  = powerlevel10k-style segment-based prompt",
            );

            // --- Segment preset (only if prompt_style = "segments") ---
            segment_preset = if prompt_style == "segments" {
                Some(pick_choice(
                    "  \u{1f3a8}  Segment preset",
                    0,
                    &["classic", "lean", "rainbow", "pure", "robbyrussell"],
                    "  \u{2502}  classic       = P10K classic layout\n  \u{2502}  lean          = P10K lean layout\n  \u{2502}  rainbow       = P10K rainbow colours\n  \u{2502}  pure          = P10K pure layout\n  \u{2502}  robbyrussell  = compact classic prompt feel",
                ))
            } else {
                None
            };

            // --- Right prompt ---
            right_prompt = pick_choice(
                "  \u{23f1}\u{fe0f}  Right-side info",
                1,
                &["off", "time", "full"],
                "  \u{2502}  off  = no right prompt\n  \u{2502}  time = show current time (HH:MM)\n  \u{2502}  full = time + git branch",
            );
            print_theme_preview(&theme, &symbol);
        }
    }

    let prompt_plugins_enabled = prompt_enabled && !theme.is_empty();

    // --- Git prompt ---
    let git_enabled = if prompt_plugins_enabled {
        prompt_yn("  \u{1f500}  Show git branch/status in the prompt", true)
    } else {
        prompt_yn("  \u{1f500}  Load Git helper aliases/functions", true)
    };
    let starship_git_enabled = prompt_plugins_enabled
        && git_enabled
        && starship_available()
        && prompt_yn(
            "  \u{1f680}  Use Starship for the Git prompt segment",
            false,
        );

    // --- Completion style ---
    let completion_style = pick_choice(
        "  \u{1f5b1}\u{fe0f}  Tab completion style",
        0,
        &["column", "list", "inline"],
        "  \u{2502}  column = multi-column grid (zsh style)\n  \u{2502}  list   = vertical list with descriptions (fish style)\n  \u{2502}  inline = insert first match, Tab cycles (bash menu-complete)",
    );

    // --- Generate shell rc ---
    let rc_content = generate_rc(
        &theme,
        &prompt_style,
        &right_prompt,
        &symbol,
        &cwd_style,
        prompt_plugins_enabled,
        git_enabled,
        starship_git_enabled,
        segment_preset.as_deref(),
        &completion_style,
    );

    // Write ~/.niubashrc
    let rc_path = home.join(PRIMARY_RC_FILE);
    let backup_path = write_primary_rc(&home, &rc_content)?;

    // Write .setup-done flag so subsequent starts skip the wizard
    let niubash_dir = home.join(".niubash");
    let _ = std::fs::create_dir_all(&niubash_dir);
    let _ = std::fs::write(niubash_dir.join(SETUP_DONE_FILE), b"");

    println!();
    println!("  \u{2705}  Shell rc written to {}", rc_path.display());
    if let Some(path) = backup_path {
        println!("  \u{1f4e6}  Previous rc backed up to {}", path.display());
    }
    if !preset_packages.is_empty() {
        println!(
            "  \u{1f4e6}  Installed packages: {}",
            preset_packages.join(", ")
        );
    }
    println!();
    println!("  \u{1f680}  You can tweak these settings any time by editing that file.");
    println!("  \u{1f501}  Run `niu setup` any time to repeat this guide.");
    println!("  \u{1f4a1}  See docs/src/getting-started.md for the full configuration reference.");
    println!();

    Ok(())
}

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Interactive choice wrapper: uses arrow-key menu when stdin is a terminal,
/// falls back to the default value otherwise. Returns the selected option string.
fn pick_choice(label: &str, default_idx: usize, options: &[&str], help: &str) -> String {
    if crate::terminal::stdio_is_interactive() {
        match interactive_menu::interactive_choice(label, options, default_idx, help) {
            Selection::Confirmed(idx) => options[idx].to_string(),
            Selection::UseDefault => options[default_idx].to_string(),
        }
    } else {
        options[default_idx].to_string()
    }
}

/// Install packages via wpm, printing progress for each.
fn install_wpm_packages(packages: &[String]) {
    println!();
    for pkg in packages {
        print!("  \u{1f527}  Installing {}... ", pkg);
        io::stdout().flush().ok();
        let status = Command::new("wpm")
            .arg("install")
            .arg(pkg)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        match status {
            Ok(s) if s.success() => println!("\u{2705}"),
            Ok(_) => println!("\u{26a0}\u{fe0f}  (not found or failed)"),
            Err(_) => println!("\u{26a0}\u{fe0f}  (wpm not available)"),
        }
    }
    println!();
}

fn setup_home_dir() -> PathBuf {
    shell_home_dir().unwrap_or_else(|| PathBuf::from("."))
}

fn generate_rc(
    theme: &str,
    prompt_style: &str,
    right_prompt: &str,
    symbol: &str,
    cwd_style: &str,
    prompt_enabled: bool,
    git_enabled: bool,
    starship_git_enabled: bool,
    segment_preset: Option<&str>,
    completion_style: &str,
) -> String {
    let theme = if prompt_enabled { theme } else { "" };
    let symbol = if prompt_enabled { symbol } else { ">" };
    let (prompt_template, right_template) = if prompt_style == "segments" {
        match segment_preset.unwrap_or("classic") {
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
        match (prompt_style, right_prompt) {
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
    let prompt_template = if git_enabled {
        prompt_template
    } else {
        strip_git_prompt_tokens(&prompt_template)
    };
    let right_template = if git_enabled {
        right_template
    } else {
        strip_git_prompt_tokens(&right_template)
    };
    let theme_plugin = if prompt_enabled {
        theme_plugin_name(theme)
    } else {
        String::new()
    };
    let plugins = plugin_load_shell_array(prompt_enabled, git_enabled, starship_git_enabled);
    let starship_segment_setup = if starship_git_enabled {
        "NIU_STARSHIP_SEGMENTS=git\nexport NIU_STARSHIP_SEGMENTS\n".to_string()
    } else {
        String::new()
    };
    let segment_note = segment_preset
        .map(|preset| format!("# Segment preset selected during setup: {preset}\n"))
        .unwrap_or_default();
    let prompt_call = if prompt_enabled {
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
    if [ -f "$__niubash_bundle/oh-my-niu.winux" ]; then
      NIUBASH="$__niubash_bundle"
      export NIUBASH
      break
    fi
  done
fi

if [ -f "$NIUBASH/oh-my-niu.winux" ]; then
  . "$NIUBASH/oh-my-niu.winux"
fi

{}
unset __niubash_bundle __niubash_home_drive __niubash_home_rest
"#,
        shell_quote(theme),
        shell_quote(&theme_plugin),
        shell_quote(symbol),
        shell_quote(cwd_style),
        shell_quote(completion_style),
        plugins,
        starship_segment_setup,
        segment_note,
        prompt_call,
    )
}

fn plugin_load_shell_array(
    prompt_enabled: bool,
    git_enabled: bool,
    starship_git_enabled: bool,
) -> String {
    let mut plugins = Vec::new();
    if prompt_enabled {
        plugins.push("prompt-core".to_string());
    }
    if git_enabled {
        plugins.push("git".to_string());
    }
    if starship_git_enabled {
        plugins.push("starship".to_string());
    }
    format!("({})", plugins.join(" "))
}

fn starship_available() -> bool {
    Command::new("starship")
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
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

fn print_theme_previews(themes: &[&str], symbol: &str) {
    println!("  \u{2502}  Theme previews:");
    for theme_name in themes {
        println!("  \u{2502}    {}", theme_preview_line(theme_name, symbol));
    }
}

fn print_theme_preview(theme_name: &str, symbol: &str) {
    println!();
    println!("  \u{2502}  Selected preview:");
    println!("  \u{2502}    {}", theme_preview_line(theme_name, symbol));
    println!();
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

fn prompt_yn(label: &str, default: bool) -> bool {
    let default_str = if default { "Y/n" } else { "y/N" };
    print!("  {} [{}]: ", label, default_str);
    io::stdout().flush().ok();
    let mut input = String::new();
    io::stdin().read_line(&mut input).ok();
    let input = input.trim().to_lowercase();

    match input.as_str() {
        "y" | "yes" | "true" => true,
        "n" | "no" | "false" => false,
        _ => default,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::PROCESS_STATE_LOCK;

    #[test]
    fn display_welcome_side_by_side_renders_without_panic() {
        display_welcome_side_by_side(false);
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

    #[test]
    fn generated_rc_uses_shell_entrypoint_not_toml_sections() {
        let rc = generate_rc(
            "minimal", "minimal", "time", ">", "home", true, true, false, None, "column",
        );

        assert!(rc.contains("NIU_THEME_PLUGIN='theme-minimal'"));
        assert!(rc.contains("NIU_PROMPT_CWD_STYLE='home'"));
        assert!(rc.contains("NIU_DISABLE_DEFAULT_PLUGINS=1"));
        assert!(rc.contains("NIU_PLUGINS=(prompt-core git)"));
        assert!(rc.contains("\"$NIU_APP_BUNDLE_PATH\""));
        assert!(rc.contains(". \"$NIUBASH/oh-my-niu.winux\""));
        assert!(rc.contains("niubash_prompt_use_template '{cwd} ' '{time} '"));
        assert!(!rc.contains("[plugins]"));
        assert!(!rc.contains("[shell]"));
        assert!(!rc.contains("prompt_format ="));
    }

    #[test]
    fn generated_rc_can_disable_git_prompt_tokens() {
        let rc = generate_rc(
            "minimal", "classic", "full", "$", "full", true, false, false, None, "column",
        );

        assert!(rc.contains("NIU_THEME_PLUGIN='theme-minimal'"));
        assert!(rc.contains("NIU_PROMPT_CWD_STYLE='full'"));
        assert!(rc.contains("NIU_PLUGINS=(prompt-core)"));
        assert!(!rc.contains(" git "));
        assert!(!rc.contains("{git_prompt}"));
        assert!(!rc.contains("{git_branch}"));
    }

    #[test]
    fn generated_rc_can_disable_prompt_theme_plugins() {
        let rc = generate_rc(
            "", "off", "off", ">", "basename", false, true, false, None, "column",
        );

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
        let rc = generate_rc(
            "spaceship",
            "multiline",
            "off",
            "%",
            "home",
            true,
            true,
            true,
            None,
            "column",
        );

        assert!(rc.contains("NIU_THEME_PLUGIN='theme-spaceship'"));
        assert!(rc.contains("NIU_PLUGINS=(prompt-core git starship)"));
        assert!(rc.contains("NIU_STARSHIP_SEGMENTS=git"));
        assert!(rc.contains("niubash_prompt_use_template"));
        assert!(rc.contains("{git}"));
        assert!(!rc.contains("NIU_PROMPT_BACKEND"));
        assert!(!rc.contains("STARSHIP_CONFIG"));
    }

    #[test]
    fn generated_rc_includes_completion_style() {
        let rc = generate_rc(
            "minimal", "minimal", "off", ">", "home", true, true, false, None, "list",
        );
        assert!(rc.contains("NIU_COMPLETION_STYLE='list'"));
        assert!(rc.contains("NIU_COMPLETION_STYLE"));

        let rc = generate_rc(
            "minimal", "minimal", "off", ">", "home", true, true, false, None, "inline",
        );
        assert!(rc.contains("NIU_COMPLETION_STYLE='inline'"));
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
