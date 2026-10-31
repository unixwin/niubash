//! LS_COLORS-style candidate coloring (niubash#743).
//!
//! zsh's completion list paints each match through the same color table
//! `ls --color` uses: directories bold blue, executables bold green,
//! symlinks cyan, and the `*.ext` rules for archives/images/etc. The
//! completer attaches a [`nu_ansi_term::Style`] to each filesystem
//! candidate's `Suggestion::style`; the menus paint it (the cycle/select
//! grid directly, the stock column menu through reedline), and the
//! selected cell composes the file color with the selection style.
//!
//! The table is parsed from the session's `LS_COLORS` (`di=01;34:ln=…` —
//! the `dircolors`/`LS_COLORS` syntax, not `LSCOLORS`); when the variable
//! is unset the GNU `dircolors --print-database` defaults apply, and an
//! explicitly empty value means "no colors" exactly like GNU.

use std::collections::HashMap;

use nu_ansi_term::{Color, Style};

/// What kind of filesystem object a completion candidate names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CandidateKind {
    Directory,
    Symlink,
    Executable,
    /// Any other filesystem entry — colored by suffix rule or `fi`.
    File,
}

/// A parsed `LS_COLORS` table: the special keys plus suffix rules.
#[derive(Debug, Default)]
pub struct FileColors {
    dir: Option<Style>,
    link: Option<Style>,
    exec: Option<Style>,
    file: Option<Style>,
    pipe: Option<Style>,
    socket: Option<Style>,
    /// `*.ext` rules keyed by the lowercased extension including the
    /// dot (`".tar"`). Longest-suffix-first matching is unnecessary —
    /// LS_COLORS rules key on the final extension exactly like GNU ls.
    extensions: HashMap<String, Style>,
}

impl FileColors {
    /// The session-environment lookup used by the completer: the
    /// session's `LS_COLORS` wins; absent there, the inherited process
    /// value applies; unset everywhere falls back to the GNU default
    /// table (the same baseline `dircolors --print-database` ships).
    pub fn from_env_vars(env_vars: &HashMap<String, String>) -> Self {
        match env_vars.get("LS_COLORS").map(String::as_str) {
            Some(spec) => Self::parse(spec),
            None => match std::env::var("LS_COLORS") {
                Ok(spec) => Self::parse(&spec),
                Err(_) => Self::defaults(),
            },
        }
    }

    /// `Some(spec)` parses the table (empty spec = no colors, GNU's
    /// `LS_COLORS=""` semantics); `None` uses the GNU default table.
    pub fn resolve(spec: Option<&str>) -> Self {
        match spec {
            Some(spec) => Self::parse(spec),
            None => Self::defaults(),
        }
    }

    /// Parse `di=01;34:ln=01;36:*.tar=01;31:…`. Unrecognized keys and
    /// malformed styles are skipped — a partial table still colors
    /// what it declares.
    pub fn parse(spec: &str) -> Self {
        let mut colors = Self::default();
        for pair in spec.split(':') {
            let Some((key, value)) = pair.split_once('=') else {
                continue;
            };
            let Some(style) = parse_style(value) else {
                continue;
            };
            match key {
                "di" => colors.dir = Some(style),
                "ln" => colors.link = Some(style),
                "ex" => colors.exec = Some(style),
                "fi" => colors.file = Some(style),
                "pi" => colors.pipe = Some(style),
                "so" => colors.socket = Some(style),
                // `or` (orphan link) falls back to `ln` below — GNU
                // shows the orphan style only for broken links, which
                // a directory walk cannot tell apart cheaply.
                _ => {
                    if let Some(ext) = key.strip_prefix('*') {
                        colors.extensions.insert(ext.to_ascii_lowercase(), style);
                    }
                }
            }
        }
        colors
    }

    /// The GNU `dircolors` builtin table, trimmed to the keys a
    /// completion grid renders meaningfully: file kinds and the common
    /// archive/image/media suffix groups.
    pub fn defaults() -> Self {
        const DEFAULTS: &str = concat!(
            "di=01;34:ln=01;36:ex=01;32:pi=33:so=01;35:",
            "*.7z=01;31:*.aac=01;36:*.avi=01;35:*.bmp=01;35:*.bz=01;31:",
            "*.bz2=01;31:*.deb=01;31:*.dll=01;31:*.dmg=01;35:*.flac=01;36:",
            "*.flv=01;35:*.gif=01;35:*.gz=01;31:*.ico=01;35:*.iso=01;35:",
            "*.jpeg=01;35:*.jpg=01;35:*.m2v=01;35:*.m4v=01;35:*.mkv=01;35:",
            "*.mov=01;35:*.mp3=01;36:*.mp4=01;35:*.mpeg=01;35:*.mpg=01;35:",
            "*.ogg=01;36:*.ogm=01;35:*.png=01;35:*.rar=01;31:*.rpm=01;31:",
            "*.svg=01;35:*.tar=01;31:*.tbz=01;31:*.tbz2=01;31:*.tgz=01;31:",
            "*.tif=01;35:*.tiff=01;35:*.vob=01;35:*.wav=01;36:*.webm=01;35:",
            "*.webp=01;35:*.wmv=01;35:*.xz=01;31:*.zip=01;31:*.zst=01;31"
        );
        Self::parse(DEFAULTS)
    }

    /// Whether the table paints anything at all — an explicitly empty
    /// `LS_COLORS` must not colorize (GNU treats it as "no colors").
    pub fn is_empty(&self) -> bool {
        self.dir.is_none()
            && self.link.is_none()
            && self.exec.is_none()
            && self.file.is_none()
            && self.extensions.is_empty()
    }

    /// The style a candidate of `kind` named `name` paints with, or
    /// `None` when the table has no rule for it.
    pub fn style_for(&self, kind: CandidateKind, name: &str) -> Option<Style> {
        match kind {
            CandidateKind::Directory => self.dir,
            CandidateKind::Symlink => self.link,
            CandidateKind::Executable => self.exec,
            CandidateKind::File => self
                .extension_of(name)
                .and_then(|ext| self.extensions.get(&ext.to_ascii_lowercase()).copied())
                .or(self.file),
        }
    }

    /// The style for a pipe/socket candidate (the simulated /dev
    /// namespace is the only producer today).
    pub fn style_for_special(&self, kind: SpecialKind) -> Option<Style> {
        match kind {
            SpecialKind::Pipe => self.pipe,
            SpecialKind::Socket => self.socket,
        }
    }

    fn extension_of<'a>(&self, name: &'a str) -> Option<&'a str> {
        let name = name.trim_end_matches(['/', '\\']);
        let dot = name.rfind('.')?;
        // `..foo` / a leading dot is not an extension (GNU's rule).
        if dot == 0 || name[..dot].ends_with(['/', '\\']) {
            return None;
        }
        Some(&name[dot..])
    }
}

/// Non-regular-file kinds with dedicated LS_COLORS keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpecialKind {
    Pipe,
    Socket,
}

/// Parse one LS_COLORS style spec (`01;34`, `38;5;203`, `38;2;12;34;56`)
/// into a [`Style`]. Empty or all-unknown codes yield `None`.
fn parse_style(spec: &str) -> Option<Style> {
    let mut style = Style::new();
    let mut touched = false;
    let mut codes = spec.split(';').peekable();
    while let Some(code) = codes.next() {
        match code {
            "00" | "0" | "" => style = Style::new(),
            "01" | "1" => style = style.bold(),
            "02" | "2" => style = style.dimmed(),
            "03" | "3" => style = style.italic(),
            "04" | "4" => style = style.underline(),
            "05" | "5" | "06" | "6" => style = style.blink(),
            "07" | "7" => style = style.reverse(),
            "08" | "8" => style = style.hidden(),
            "09" | "9" => style = style.strikethrough(),
            "38" => match codes.next() {
                Some("5") => {
                    if let Some(n) = codes.next().and_then(|v| v.parse::<u8>().ok()) {
                        style = style.fg(Color::Fixed(n));
                    }
                }
                Some("2") => {
                    let r = codes.next().and_then(|v| v.parse::<u8>().ok());
                    let g = codes.next().and_then(|v| v.parse::<u8>().ok());
                    let b = codes.next().and_then(|v| v.parse::<u8>().ok());
                    if let (Some(r), Some(g), Some(b)) = (r, g, b) {
                        style = style.fg(Color::Rgb(r, g, b));
                    }
                }
                _ => {}
            },
            "48" => match codes.next() {
                Some("5") => {
                    if let Some(n) = codes.next().and_then(|v| v.parse::<u8>().ok()) {
                        style = style.on(Color::Fixed(n));
                    }
                }
                Some("2") => {
                    let r = codes.next().and_then(|v| v.parse::<u8>().ok());
                    let g = codes.next().and_then(|v| v.parse::<u8>().ok());
                    let b = codes.next().and_then(|v| v.parse::<u8>().ok());
                    if let (Some(r), Some(g), Some(b)) = (r, g, b) {
                        style = style.on(Color::Rgb(r, g, b));
                    }
                }
                _ => {}
            },
            _ => {
                if let Ok(n) = code.parse::<u8>() {
                    match n {
                        30..=37 => style = style.fg(basic_color(n - 30, false)),
                        39 => style.foreground = None,
                        40..=47 => style = style.on(basic_color(n - 40, false)),
                        49 => style.background = None,
                        90..=97 => style = style.fg(basic_color(n - 90, true)),
                        100..=107 => style = style.on(basic_color(n - 100, true)),
                        _ => continue,
                    }
                } else {
                    continue;
                }
            }
        }
        touched = true;
    }
    touched.then_some(style)
}

/// The 8 standard colors; `bright` maps to the light variants (90–97 /
/// 100–107 codes).
fn basic_color(index: u8, bright: bool) -> Color {
    match (index, bright) {
        (0, false) => Color::Black,
        (0, true) => Color::DarkGray,
        (1, false) => Color::Red,
        (1, true) => Color::LightRed,
        (2, false) => Color::Green,
        (2, true) => Color::LightGreen,
        (3, false) => Color::Yellow,
        (3, true) => Color::LightYellow,
        (4, false) => Color::Blue,
        (4, true) => Color::LightBlue,
        (5, false) => Color::Purple,
        (5, true) => Color::LightPurple,
        (6, false) => Color::Cyan,
        (6, true) => Color::LightCyan,
        (7, false) => Color::White,
        (7, true) => Color::LightGray,
        _ => Color::Fixed(index),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_basic_styles() {
        let colors = FileColors::parse("di=01;34:ln=01;36:ex=01;32:*.tar=01;31");
        let dir = colors.style_for(CandidateKind::Directory, "x").unwrap();
        assert!(dir.is_bold);
        assert_eq!(dir.foreground, Some(Color::Blue));
        let link = colors.style_for(CandidateKind::Symlink, "x").unwrap();
        assert_eq!(link.foreground, Some(Color::Cyan));
        let exec = colors.style_for(CandidateKind::Executable, "x").unwrap();
        assert_eq!(exec.foreground, Some(Color::Green));
        let tar = colors.style_for(CandidateKind::File, "a.tar").unwrap();
        assert!(tar.is_bold);
        assert_eq!(tar.foreground, Some(Color::Red));
        // A plain file with no `fi` key gets no style.
        assert!(colors.style_for(CandidateKind::File, "a.txt").is_none());
    }

    #[test]
    fn extension_match_is_case_insensitive_and_final_only() {
        let colors = FileColors::parse("*.TAR=31");
        assert!(colors.style_for(CandidateKind::File, "a.tar").is_some());
        assert!(colors.style_for(CandidateKind::File, "a.TAR").is_some());
        assert!(colors.style_for(CandidateKind::File, ".hidden").is_none());
    }

    #[test]
    fn extended_and_bright_color_codes() {
        let fixed = parse_style("38;5;203").unwrap();
        assert_eq!(fixed.foreground, Some(Color::Fixed(203)));
        let rgb = parse_style("38;2;1;2;3").unwrap();
        assert_eq!(rgb.foreground, Some(Color::Rgb(1, 2, 3)));
        let bright = parse_style("91").unwrap();
        assert_eq!(bright.foreground, Some(Color::LightRed));
        let bright_bg = parse_style("102").unwrap();
        assert_eq!(bright_bg.background, Some(Color::LightGreen));
        let reset_then_color = parse_style("0;32").unwrap();
        assert!(!reset_then_color.is_bold);
        assert_eq!(reset_then_color.foreground, Some(Color::Green));
    }

    #[test]
    fn empty_spec_means_no_colors_and_absent_means_defaults() {
        assert!(FileColors::parse("").is_empty());
        assert!(FileColors::resolve(Some("")).is_empty());
        assert!(!FileColors::resolve(None).is_empty());
        let defaults = FileColors::defaults();
        assert!(defaults.style_for(CandidateKind::Directory, "x").is_some());
        assert!(defaults.style_for(CandidateKind::File, "a.zip").is_some());
    }
}
