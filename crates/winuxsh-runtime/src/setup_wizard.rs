//! First-run setup wizard.
//!
//! Guides the user through initial interactive configuration, then writes a
//! normal shell rc file.

use std::io::{self, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::path_utils::shell_home_dir;
use crate::theme;

const PRIMARY_RC_FILE: &str = ".winuxshrc";
const LEGACY_RC_FILE: &str = ".winshrc";
const SETUP_DONE_FILE: &str = ".setup-done";

/// Returns `true` if the user has never run the setup wizard before
/// (i.e. no primary/legacy rc file and no setup marker). Legacy/managed TOML
/// metadata no longer blocks first-run rc onboarding.
pub fn is_first_run() -> bool {
    let home = setup_home_dir();
    for name in [PRIMARY_RC_FILE, LEGACY_RC_FILE] {
        if home.join(name).is_file() {
            return false;
        }
    }
    !home.join(".winuxsh").join(SETUP_DONE_FILE).is_file()
}

/// Run the interactive setup wizard.
///
/// Prints a welcome banner, asks the user a few questions with defaults,
/// writes `~/.winuxshrc`, and creates the `.setup-done` marker.
pub fn run_wizard() -> anyhow::Result<()> {
    run_wizard_inner(false)
}

/// Re-run the setup wizard even if the user already has a startup rc.
pub fn rerun_wizard() -> anyhow::Result<()> {
    run_wizard_inner(true)
}

fn run_wizard_inner(reconfigure: bool) -> anyhow::Result<()> {
    let home = setup_home_dir();

    println!();
    println!(
        " {}  Welcome to Winuxsh {}!",
        "\u{1f389}",
        env!("CARGO_PKG_VERSION")
    );
    println!(
        " {}  A bash-compatible shell for Windows \u{2014} no WSL, no MSYS2 required.",
        "\u{2728}"
    );
    println!();
    if reconfigure {
        println!("  Reconfigure your interactive prompt/plugins. Existing rc will be backed up.");
    } else {
        println!("  Let\u{2019}s get you set up.  (Press Enter to accept defaults.)");
    }
    println!();

    let prompt_enabled = prompt_yn("  \u{1f3b5}  Enable bundled prompt/theme plugins", true);

    let mut theme = String::new();
    let mut prompt_style = "off".to_string();
    let mut right_prompt = "off".to_string();
    let mut symbol = ">".to_string();
    let mut segment_preset: Option<String> = None;
    let cwd_style = prompt_choice(
        "  \u{1f4c1}  Prompt path display",
        "home",
        &["home", "full", "basename"],
        "  \u{2502}  home     = ~ and ~/repo below your profile\n  \u{2502}  full     = C:/Users/name/repo\n  \u{2502}  basename = only the current directory name",
    );

    if prompt_enabled {
        // --- Theme ---
        let theme_list = theme::list_available_names();
        if theme_list.is_empty() {
            anyhow::bail!(
                "No Winuxsh themes found. The oh-my-winuxsh bundle is required and should be preinstalled."
            );
        }
        let theme_refs: Vec<&str> = theme_list.iter().map(String::as_str).collect();
        let default_theme = if theme_refs.contains(&"p10-classic") {
            "p10-classic"
        } else if theme_refs.contains(&"minimal") {
            "minimal"
        } else {
            theme_refs.first().copied().unwrap_or("default")
        };
        print_theme_previews(&theme_refs, "\u{276f}");
        theme = prompt_choice(
            "  \u{1f3a8}  Colour theme",
            default_theme,
            &theme_refs,
            "  \u{2502}  Choose the plugin-owned prompt colour scheme. Official themes come from oh-my-winuxsh.",
        );

        // --- Prompt symbol ---
        symbol = prompt_choice(
            "  \u{1f3b5}  Prompt symbol",
            "\u{276f}",
            &["\u{276f}", "\u{3bb}", "\u{25b6}", "\u{24}", "%"],
            "  \u{2502}  Pick the character that ends your prompt line.\n  \u{2502}  \u{276f} heavy right-pointing angle (powerlevel10k style)\n  \u{2502}  \u{3bb} lambda (functional/minimal)\n  \u{2502}  \u{25b6} black right-pointing triangle\n  \u{2502}  $ dollar sign (classic bash)\n  \u{2502}  % percent sign (classic fish)",
        );

        // --- Prompt style ---
        prompt_style = prompt_choice(
             "  \u{1f3b5}  Prompt style",
             "minimal",
            &["minimal", "classic", "powerline", "multiline", "segments"],
            "  \u{2502}  minimal   = cwd git prompt_char\n  \u{2502}  classic   = user@host cwd git prompt_char\n  \u{2502}  powerline = compact left prompt with right-side info\n  \u{2502}  multiline = first line context, second line cwd/git\n  \u{2502}  segments  = powerlevel10k-style segment-based prompt",
         );

        // --- Segment preset (only if prompt_style = "segments") ---
        segment_preset = if prompt_style == "segments" {
            Some(prompt_choice(
                "  \u{1f3a8}  Segment preset",
                "classic",
                &["classic", "lean", "rainbow", "pure", "robbyrussell"],
                "  \u{2502}  classic       = P10K classic layout\n  \u{2502}  lean          = P10K lean layout\n  \u{2502}  rainbow       = P10K rainbow colours\n  \u{2502}  pure          = P10K pure layout\n  \u{2502}  robbyrussell  = compact classic prompt feel",
            ))
        } else {
            None
        };

        // --- Right prompt ---
        right_prompt = prompt_choice(
             "  \u{23f1}\u{fe0f}  Right-side info",
            "time",
            &["off", "time", "full"],
            "  \u{2502}  off  = no right prompt\n  \u{2502}  time = show current time (HH:MM)\n  \u{2502}  full = time + git branch",
        );
        print_theme_preview(&theme, &symbol);
    }

    // --- Git prompt ---
    let git_enabled = if prompt_enabled {
        prompt_yn("  \u{1f500}  Show git branch/status in the prompt", true)
    } else {
        prompt_yn("  \u{1f500}  Load Git helper aliases/functions", true)
    };
    let starship_git_enabled = prompt_enabled
        && git_enabled
        && starship_available()
        && prompt_yn(
            "  \u{1f680}  Use Starship for the Git prompt segment",
            false,
        );

    // --- Generate shell rc ---
    let rc_content = generate_rc(
        &theme,
        &prompt_style,
        &right_prompt,
        &symbol,
        &cwd_style,
        prompt_enabled,
        git_enabled,
        starship_git_enabled,
        segment_preset.as_deref(),
    );

    // Write ~/.winuxshrc
    let rc_path = home.join(PRIMARY_RC_FILE);
    let backup_path = write_primary_rc(&home, &rc_content)?;

    // Write .setup-done flag so subsequent starts skip the wizard
    let winuxsh_dir = home.join(".winuxsh");
    let _ = std::fs::create_dir_all(&winuxsh_dir);
    let _ = std::fs::write(winuxsh_dir.join(SETUP_DONE_FILE), b"");

    println!();
    println!("  \u{2705}  Shell rc written to {}", rc_path.display());
    if let Some(path) = backup_path {
        println!("  \u{1f4e6}  Previous rc backed up to {}", path.display());
    }
    println!();
    println!("  \u{1f680}  You can tweak these settings any time by editing that file.");
    println!("  \u{1f501}  Run `winuxsh setup` any time to repeat this guide.");
    println!("  \u{1f4a1}  See docs/src/getting-started.md for the full configuration reference.");
    println!();

    Ok(())
}

// ── Helpers ──────────────────────────────────────────────────────────────────

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
        "WINUXSH_STARSHIP_SEGMENTS=git\nexport WINUXSH_STARSHIP_SEGMENTS\n".to_string()
    } else {
        String::new()
    };
    let segment_note = segment_preset
        .map(|preset| format!("# Segment preset selected during setup: {preset}\n"))
        .unwrap_or_default();
    let prompt_call = if prompt_enabled {
        format!(
            "winuxsh_prompt_use_template {} {} 2>/dev/null || true\n",
            shell_quote(&prompt_template),
            shell_quote(&right_template)
        )
    } else {
        "# Prompt/theme plugins disabled by setup.\n".to_string()
    };
    format!(
        r#"# Winuxsh interactive rc — generated by the setup wizard.
# Edit this file with normal Winuxsh/bash syntax.
# Structured TOML manifests are not user startup configuration; new interactive setup
# should live here.

WINUXSH_THEME={}
WINUXSH_THEME_PLUGIN={}
WINUXSH_PROMPT_SYMBOL={}
WINUXSH_PROMPT_CWD_STYLE={}
WINUXSH_DISABLE_DEFAULT_PLUGINS=1
export WINUXSH_THEME WINUXSH_THEME_PLUGIN WINUXSH_PROMPT_SYMBOL
export WINUXSH_PROMPT_CWD_STYLE WINUXSH_DISABLE_DEFAULT_PLUGINS

WINUXSH_PLUGINS={}
{}
{}
if [ -z "${{HOME:-}}" ] && [ -n "${{USERPROFILE:-}}" ]; then
  case "$USERPROFILE" in
    /[A-Za-z]/*)
      __winuxsh_home_drive="${{USERPROFILE#/}}"
      __winuxsh_home_drive="${{__winuxsh_home_drive%%/*}}"
      __winuxsh_home_rest="${{USERPROFILE#/$__winuxsh_home_drive/}}"
      HOME="$__winuxsh_home_drive:/$__winuxsh_home_rest"
      ;;
    *)
      HOME="${{USERPROFILE//\\//}}"
      ;;
  esac
  export HOME
fi

if [ -z "${{WINUXSH:-}}" ]; then
  for __winuxsh_bundle in "$HOME/.oh-my-winuxsh" "$HOME/.winuxsh/oh-my-winuxsh" "$HOME/.winuxsh/bundles/oh-my-winuxsh"/* "$WINUXSH_APP_BUNDLE_PATH"; do
    if [ -f "$__winuxsh_bundle/oh-my-winuxsh.winux" ]; then
      WINUXSH="$__winuxsh_bundle"
      export WINUXSH
      break
    fi
  done
fi

if [ -f "$WINUXSH/oh-my-winuxsh.winux" ]; then
  . "$WINUXSH/oh-my-winuxsh.winux"
fi

{}
unset __winuxsh_bundle __winuxsh_home_drive __winuxsh_home_rest
"#,
        shell_quote(theme),
        shell_quote(&theme_plugin),
        shell_quote(symbol),
        shell_quote(cwd_style),
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
    let winuxsh_dir = home.join(".winuxsh");
    std::fs::create_dir_all(&winuxsh_dir)?;
    let stamp = timestamp_id();
    let tmp_path = winuxsh_dir.join(format!(".winuxshrc.tmp-{stamp}"));
    std::fs::write(&tmp_path, rc_content)?;

    let backup_path = if rc_path.is_file() {
        let backup_dir = winuxsh_dir.join("backups");
        std::fs::create_dir_all(&backup_dir)?;
        let backup = backup_dir.join(format!(".winuxshrc.{stamp}.bak"));
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
    let dir = theme.prompt_dir.paint("~/repo/winuxsh").to_string();
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

fn prompt_choice(label: &str, default: &str, options: &[&str], help: &str) -> String {
    println!("{}", label);
    for line in help.lines() {
        println!("{}", line);
    }

    let default_idx = options.iter().position(|o| *o == default).unwrap_or(0);
    let default_display = default_idx + 1;

    loop {
        print!(
            "  |  Enter choice [1-{} / Enter for {}]: ",
            options.len(),
            default_display
        );
        io::stdout().flush().ok();
        let mut input = String::new();
        io::stdin().read_line(&mut input).ok();
        let input = input.trim().to_lowercase();

        if input.is_empty() {
            return default.to_string();
        }

        if let Ok(idx) = input.parse::<usize>() {
            if idx >= 1 && idx <= options.len() {
                return options[idx - 1].to_string();
            }
        }

        if options.contains(&input.as_str()) {
            return input;
        }

        println!(
            "  |  Enter a number 1-{} (or Enter for {}).",
            options.len(),
            default_display
        );
    }
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
    fn setup_home_dir_accepts_shell_style_home_env() {
        let _process_lock = PROCESS_STATE_LOCK.lock().unwrap();
        let home = unique_temp_dir("winuxsh-setup-home").join("home");
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
            "minimal", "minimal", "time", ">", "home", true, true, false, None,
        );

        assert!(rc.contains("WINUXSH_THEME_PLUGIN='theme-minimal'"));
        assert!(rc.contains("WINUXSH_PROMPT_CWD_STYLE='home'"));
        assert!(rc.contains("WINUXSH_DISABLE_DEFAULT_PLUGINS=1"));
        assert!(rc.contains("WINUXSH_PLUGINS=(prompt-core git)"));
        assert!(rc.contains("\"$WINUXSH_APP_BUNDLE_PATH\""));
        assert!(rc.contains(". \"$WINUXSH/oh-my-winuxsh.winux\""));
        assert!(rc.contains("winuxsh_prompt_use_template '{cwd} ' '{time} '"));
        assert!(!rc.contains("[plugins]"));
        assert!(!rc.contains("[shell]"));
        assert!(!rc.contains("prompt_format ="));
    }

    #[test]
    fn generated_rc_can_disable_git_prompt_tokens() {
        let rc = generate_rc(
            "minimal", "classic", "full", "$", "full", true, false, false, None,
        );

        assert!(rc.contains("WINUXSH_THEME_PLUGIN='theme-minimal'"));
        assert!(rc.contains("WINUXSH_PROMPT_CWD_STYLE='full'"));
        assert!(rc.contains("WINUXSH_PLUGINS=(prompt-core)"));
        assert!(!rc.contains(" git "));
        assert!(!rc.contains("{git_prompt}"));
        assert!(!rc.contains("{git_branch}"));
    }

    #[test]
    fn generated_rc_can_disable_prompt_theme_plugins() {
        let rc = generate_rc("", "off", "off", ">", "basename", false, true, false, None);

        assert!(rc.contains("WINUXSH_THEME=''"));
        assert!(rc.contains("WINUXSH_THEME_PLUGIN=''"));
        assert!(rc.contains("WINUXSH_PROMPT_CWD_STYLE='basename'"));
        assert!(rc.contains("WINUXSH_DISABLE_DEFAULT_PLUGINS=1"));
        assert!(rc.contains("WINUXSH_PLUGINS=(git)"));
        assert!(rc.contains("# Prompt/theme plugins disabled by setup."));
        assert!(!rc.contains("winuxsh_prompt_use_template"));
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
        );

        assert!(rc.contains("WINUXSH_THEME_PLUGIN='theme-spaceship'"));
        assert!(rc.contains("WINUXSH_PLUGINS=(prompt-core git starship)"));
        assert!(rc.contains("WINUXSH_STARSHIP_SEGMENTS=git"));
        assert!(rc.contains("winuxsh_prompt_use_template"));
        assert!(rc.contains("{git}"));
        assert!(!rc.contains("WINUXSH_PROMPT_BACKEND"));
        assert!(!rc.contains("STARSHIP_CONFIG"));
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
