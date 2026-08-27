//! Theme API for winuxsh.
//!
//! Winuxsh core owns the style schema and resolution hooks. Official themes
//! live in external bundles such as oh-my-winuxsh. Core does not ship
//! selectable official themes.

use std::path::{Path, PathBuf};

use nu_ansi_term::{Color, Style};
use serde::{Deserialize, Serialize};

/// A colour theme for the shell.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct Theme {
    pub name: String,
    pub prompt_user: Style,
    pub prompt_host: Style,
    pub prompt_dir: Style,
    pub prompt_symbol: Style,
    pub error: Style,
    pub warning: Style,
    pub success: Style,
    pub git_clean: Style,
    pub git_dirty: Style,
    pub git_status_detail: Style,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize)]
pub struct UserThemeEntry {
    pub name: String,
    pub path: PathBuf,
}

impl Theme {
    pub fn default_theme() -> Self {
        Self {
            name: "unstyled".to_string(),
            prompt_user: Style::new().bold().fg(Color::Default),
            prompt_host: Style::new().bold().fg(Color::Default),
            prompt_dir: Style::new().bold().fg(Color::Default),
            prompt_symbol: Style::new().fg(Color::Default),
            error: Style::new().fg(Color::Red),
            warning: Style::new().fg(Color::Yellow),
            success: Style::new().fg(Color::Green),
            git_clean: Style::new().fg(Color::Green),
            git_dirty: Style::new().fg(Color::Yellow),
            git_status_detail: Style::new().fg(Color::Cyan),
        }
    }
}

/// Look up a theme by name.
pub fn by_name(name: &str) -> Theme {
    if let Some(theme) = load_user_theme(name) {
        return theme;
    }

    if let Some(theme) = crate::plugins::plugin_theme(name) {
        return theme;
    }

    log::warn!(
        "Theme '{}' not found in user themes or active bundles; oh-my-winuxsh theme bundle may be missing or invalid",
        name
    );
    Theme::default_theme()
}

/// Core ships no official theme names. Official themes are discovered from
/// user theme files and active plugin bundles.
pub fn list_names() -> &'static [&'static str] {
    &[]
}

/// List user and active bundle theme names.
pub fn list_available_names() -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for entry in user_theme_entries() {
        if !names
            .iter()
            .any(|known| known.eq_ignore_ascii_case(&entry.name))
        {
            names.push(entry.name);
        }
    }
    for name in crate::plugins::plugin_theme_names() {
        if !names.iter().any(|known| known.eq_ignore_ascii_case(&name)) {
            names.push(name);
        }
    }
    names
}

fn load_user_theme(name: &str) -> Option<Theme> {
    let theme_dir = user_theme_dir()?;
    load_user_theme_from_dir(name, &theme_dir)
}

fn user_theme_dir() -> Option<PathBuf> {
    crate::path_utils::shell_home_dir().map(|home| home.join(".winuxsh").join("themes"))
}

pub fn user_theme_entries() -> Vec<UserThemeEntry> {
    user_theme_dir()
        .map(|theme_dir| user_theme_entries_from_dir(&theme_dir))
        .unwrap_or_default()
}

fn user_theme_entries_from_dir(theme_dir: &Path) -> Vec<UserThemeEntry> {
    let Ok(entries) = std::fs::read_dir(theme_dir) else {
        return Vec::new();
    };
    let mut themes = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("toml"))
        .filter_map(|path| {
            let name = path.file_stem()?.to_str()?.to_string();
            if !is_safe_theme_name(&name) || load_theme_from_file(&name, &path).is_none() {
                return None;
            }
            Some(UserThemeEntry { name, path })
        })
        .collect::<Vec<_>>();
    themes.sort_by(|a, b| a.name.cmp(&b.name));
    themes
}

fn load_user_theme_from_dir(name: &str, theme_dir: &Path) -> Option<Theme> {
    if !is_safe_theme_name(name) {
        log::warn!("Ignoring unsafe theme name '{}'", name);
        return None;
    }

    let path = theme_dir.join(format!("{}.toml", name));
    if !path.is_file() {
        return None;
    }

    load_theme_from_file(name, &path)
}

pub(crate) fn load_theme_from_file(name: &str, path: &Path) -> Option<Theme> {
    let content = match std::fs::read_to_string(&path) {
        Ok(content) => content,
        Err(e) => {
            log::warn!("Failed to read theme {}: {}", path.display(), e);
            return None;
        }
    };

    let parsed: UserThemeToml = match toml::from_str(&content) {
        Ok(parsed) => parsed,
        Err(e) => {
            log::warn!("Failed to parse theme {}: {}", path.display(), e);
            return None;
        }
    };

    parsed.into_theme(name).or_else(|| {
        log::warn!("Failed to build theme {}", path.display());
        None
    })
}

fn is_safe_theme_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
}

#[derive(Debug, Deserialize)]
struct UserThemeToml {
    prompt_user: Option<UserStyleToml>,
    prompt_host: Option<UserStyleToml>,
    prompt_dir: Option<UserStyleToml>,
    prompt_symbol: Option<UserStyleToml>,
    error: Option<UserStyleToml>,
    warning: Option<UserStyleToml>,
    success: Option<UserStyleToml>,
    git_clean: Option<UserStyleToml>,
    git_dirty: Option<UserStyleToml>,
    git_status_detail: Option<UserStyleToml>,
}

impl UserThemeToml {
    fn into_theme(self, name: &str) -> Option<Theme> {
        let mut theme = Theme::default_theme();
        theme.name = name.to_string();

        if let Some(style) = self.prompt_user {
            theme.prompt_user = style.apply_to(theme.prompt_user)?;
        }
        if let Some(style) = self.prompt_host {
            theme.prompt_host = style.apply_to(theme.prompt_host)?;
        }
        if let Some(style) = self.prompt_dir {
            theme.prompt_dir = style.apply_to(theme.prompt_dir)?;
        }
        if let Some(style) = self.prompt_symbol {
            theme.prompt_symbol = style.apply_to(theme.prompt_symbol)?;
        }
        if let Some(style) = self.error {
            theme.error = style.apply_to(theme.error)?;
        }
        if let Some(style) = self.warning {
            theme.warning = style.apply_to(theme.warning)?;
        }
        if let Some(style) = self.success {
            theme.success = style.apply_to(theme.success)?;
        }
        if let Some(style) = self.git_clean {
            theme.git_clean = style.apply_to(theme.git_clean)?;
        }
        if let Some(style) = self.git_dirty {
            theme.git_dirty = style.apply_to(theme.git_dirty)?;
        }
        if let Some(style) = self.git_status_detail {
            theme.git_status_detail = style.apply_to(theme.git_status_detail)?;
        }

        Some(theme)
    }
}

#[derive(Debug, Deserialize)]
struct UserStyleToml {
    fg: Option<String>,
    bg: Option<String>,
    bold: Option<bool>,
    italic: Option<bool>,
    underline: Option<bool>,
    dimmed: Option<bool>,
}

impl UserStyleToml {
    fn apply_to(self, mut style: Style) -> Option<Style> {
        if let Some(fg) = self.fg {
            style = style.fg(parse_color(&fg)?);
        }
        if let Some(bg) = self.bg {
            style = style.on(parse_color(&bg)?);
        }
        if let Some(bold) = self.bold {
            style.is_bold = bold;
        }
        if let Some(italic) = self.italic {
            style.is_italic = italic;
        }
        if let Some(underline) = self.underline {
            style.is_underline = underline;
        }
        if let Some(dimmed) = self.dimmed {
            style.is_dimmed = dimmed;
        }
        Some(style)
    }
}

fn parse_color(value: &str) -> Option<Color> {
    let raw = value.trim().to_ascii_lowercase();
    if let Some(hex) = raw.strip_prefix('#') {
        return parse_hex_color(hex).or_else(|| {
            log::warn!("Unknown theme color '{}'", value);
            None
        });
    }
    if raw.len() == 6 && raw.chars().all(|ch| ch.is_ascii_hexdigit()) {
        return parse_hex_color(&raw).or_else(|| {
            log::warn!("Unknown theme color '{}'", value);
            None
        });
    }
    if let Ok(number) = raw.parse::<u8>() {
        return Some(Color::Fixed(number));
    }

    let key = value
        .chars()
        .filter(|ch| *ch != '_' && *ch != '-' && !ch.is_whitespace())
        .collect::<String>()
        .to_ascii_lowercase();

    match key.as_str() {
        "black" => Some(Color::Black),
        "darkgray" | "darkgrey" => Some(Color::DarkGray),
        "red" => Some(Color::Red),
        "lightred" => Some(Color::LightRed),
        "green" => Some(Color::Green),
        "lightgreen" => Some(Color::LightGreen),
        "yellow" => Some(Color::Yellow),
        "lightyellow" => Some(Color::LightYellow),
        "blue" => Some(Color::Blue),
        "lightblue" => Some(Color::LightBlue),
        "purple" => Some(Color::Purple),
        "lightpurple" => Some(Color::LightPurple),
        "magenta" => Some(Color::Magenta),
        "lightmagenta" => Some(Color::LightMagenta),
        "cyan" => Some(Color::Cyan),
        "lightcyan" => Some(Color::LightCyan),
        "white" => Some(Color::White),
        "lightgray" | "lightgrey" => Some(Color::LightGray),
        "default" => Some(Color::Default),
        _ => {
            log::warn!("Unknown theme color '{}'", value);
            None
        }
    }
}

fn parse_hex_color(hex: &str) -> Option<Color> {
    if hex.len() != 6 {
        return None;
    }
    let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
    let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
    let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
    Some(Color::Rgb(r, g, b))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn core_exports_no_official_theme_names() {
        assert!(list_names().is_empty());
    }

    #[test]
    fn missing_theme_uses_unstyled_schema_default() {
        let theme = by_name("__missing_theme__");
        assert_eq!(theme.name, "unstyled");
    }

    #[test]
    fn loads_user_theme_from_theme_dir() {
        let dir = unique_temp_dir("winuxsh-theme-load");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("ocean.toml"),
            r#"
[prompt_user]
fg = "light cyan"
bold = false

[prompt_symbol]
fg = "light-magenta"
bold = true

[error]
fg = "red"
bold = true
"#,
        )
        .unwrap();

        let theme = load_user_theme_from_dir("ocean", &dir).unwrap();
        assert_eq!(theme.name, "ocean");
        assert_eq!(theme.prompt_user.foreground, Some(Color::LightCyan));
        assert!(!theme.prompt_user.is_bold);
        assert_eq!(theme.prompt_symbol.foreground, Some(Color::LightMagenta));
        assert!(theme.prompt_symbol.is_bold);
        assert_eq!(theme.error.foreground, Some(Color::Red));
        assert!(theme.error.is_bold);

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn invalid_user_theme_color_is_ignored() {
        let dir = unique_temp_dir("winuxsh-theme-invalid");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("broken.toml"),
            r#"
[prompt_user]
fg = "not-a-color"
"#,
        )
        .unwrap();

        assert!(load_user_theme_from_dir("broken", &dir).is_none());

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn loads_hex_background_and_style_bits_from_theme_file() {
        let dir = unique_temp_dir("winuxsh-theme-hex");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("neon.toml"),
            r##"
[prompt_user]
fg = "#ff00ff"
bg = "102030"
bold = false
italic = true
underline = true
dimmed = true
"##,
        )
        .unwrap();

        let theme = load_user_theme_from_dir("neon", &dir).unwrap();
        assert_eq!(theme.prompt_user.foreground, Some(Color::Rgb(255, 0, 255)));
        assert_eq!(theme.prompt_user.background, Some(Color::Rgb(16, 32, 48)));
        assert!(!theme.prompt_user.is_bold);
        assert!(theme.prompt_user.is_italic);
        assert!(theme.prompt_user.is_underline);
        assert!(theme.prompt_user.is_dimmed);

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn invalid_hex_theme_color_is_ignored() {
        let dir = unique_temp_dir("winuxsh-theme-invalid-hex");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("broken.toml"),
            r##"
[prompt_user]
fg = "#ff00zz"
"##,
        )
        .unwrap();

        assert!(load_user_theme_from_dir("broken", &dir).is_none());

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn user_theme_entries_from_dir_lists_valid_safe_themes() {
        let dir = unique_temp_dir("winuxsh-theme-catalog");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("market.toml"),
            r#"
[prompt_user]
fg = "green"
"#,
        )
        .unwrap();
        std::fs::write(
            dir.join("bad.name.toml"),
            r#"
[prompt_user]
fg = "green"
"#,
        )
        .unwrap();
        std::fs::write(
            dir.join("broken.toml"),
            r#"
[prompt_user]
fg = "not-a-color"
"#,
        )
        .unwrap();

        let entries = user_theme_entries_from_dir(&dir);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "market");
        assert_eq!(entries[0].path, dir.join("market.toml"));

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn unsafe_theme_names_are_ignored() {
        let dir = unique_temp_dir("winuxsh-theme-unsafe");
        std::fs::create_dir_all(&dir).unwrap();

        assert!(load_user_theme_from_dir("../dark", &dir).is_none());
        assert!(load_user_theme_from_dir("", &dir).is_none());

        let _ = std::fs::remove_dir_all(dir);
    }

    fn unique_temp_dir(prefix: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("{}-{}-{}", prefix, std::process::id(), nanos))
    }
}
