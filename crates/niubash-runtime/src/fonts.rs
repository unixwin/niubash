//! Nerd Font detection and install recommendations.
//!
//! Download retraction (owner ruling 2026-10-04): the shell carries zero
//! network/HTTP responsibility, so `niu font` no longer downloads or
//! extracts font zips. What remains is detection — scanning the per-user
//! and system font directories plus the registered font lists (HKCU and
//! HKLM on Windows) — and a recommendation surface: on Windows the winget
//! nerd-fonts packages (fonts are not part of wpm's Unix command layer,
//! so the package-manager channel for them is winget/scoop), on other
//! platforms the native package manager or nerdfonts.com. No HTTP, no
//! zip extraction, no registration writes.

use std::path::{Path, PathBuf};

/// A Nerd Font niu knows how to detect and recommend.
pub struct NerdFont {
    /// Menu label shown to the user.
    pub label: &'static str,
    /// Value a Windows Terminal profile's `font.face` should use.
    pub face: &'static str,
    /// winget package id (`winget install --id <id>`), when one exists.
    pub winget_id: Option<&'static str>,
    /// scoop manifest name in the `nerd-fonts` bucket, when one exists.
    pub scoop: Option<&'static str>,
    /// Homebrew cask name (`brew install --cask <name>`).
    pub brew_cask: &'static str,
}

/// Fonts niu detects and recommends (`niu font`, the setup wizard's
/// environment summary, `niu doctor`).
pub const FONT_OPTIONS: &[NerdFont] = &[
    NerdFont {
        label: "JetBrainsMono Nerd Font",
        face: "JetBrainsMono Nerd Font Mono",
        winget_id: Some("DEVCOM.JetBrainsMonoNerdFont"),
        scoop: Some("JetBrainsMono-NF"),
        brew_cask: "font-jetbrains-mono-nerd-font",
    },
    NerdFont {
        label: "MesloLGM Nerd Font",
        face: "MesloLGM Nerd Font Mono",
        // No winget package exists for Meslo; scoop's nerd-fonts bucket
        // and the nerdfonts.com release carry it.
        winget_id: None,
        scoop: Some("Meslo-NF"),
        brew_cask: "font-meslo-lg-nerd-font",
    },
    NerdFont {
        label: "CaskaydiaCove Nerd Font",
        face: "CaskaydiaCove Nerd Font Mono",
        winget_id: None,
        scoop: Some("CascadiaCode-NF"),
        brew_cask: "font-caskaydia-cove-nerd-font",
    },
];

/// Menu labels for the font choice question (plus room for a Skip entry
/// appended by the caller).
pub fn menu_labels() -> Vec<String> {
    FONT_OPTIONS.iter().map(|f| f.label.to_string()).collect()
}

/// The copy-pasteable install commands for one font: winget first on
/// Windows (fonts are a package-manager channel, not wpm's Unix command
/// layer), scoop as the Windows alternative, brew on macOS, and always
/// nerdfonts.com as the channel-free fallback.
fn install_lines(font: &NerdFont) -> Vec<String> {
    let mut lines = Vec::new();
    if let Some(id) = font.winget_id {
        lines.push(format!("winget install --id {id}    # Windows"));
    }
    if let Some(name) = font.scoop {
        lines.push(format!(
            "scoop bucket add nerd-fonts; scoop install {name}   # Windows (alternative)"
        ));
    }
    lines.push(format!("brew install --cask {}   # macOS", font.brew_cask));
    lines.push("https://www.nerdfonts.com/font-downloads   # manual download".to_string());
    lines
}

/// True when any installed font name or file looks like a Nerd Font.
/// Checks the per-user and system font directories plus the registry
/// font lists (HKCU and HKLM), and — on Termux — the single terminal
/// font file, whose Nerd-ness lives in its name table, not its name.
pub fn nerd_font_installed() -> bool {
    font_dirs().iter().any(|dir| dir_has_nerd_font(dir))
        || registry_has_nerd_font()
        || termux_font_is_nerd_font()
}

/// `niu font` — report Nerd Font detection and print per-font install
/// recommendations. Non-interactive by design (no terminal needed, no
/// network used): the command observes and advises, it never installs.
pub fn run_font_command() -> std::result::Result<(), anyhow::Error> {
    println!(
        "{}",
        crate::text_style::bold("Nerd Fonts — detection & install recommendations")
    );
    println!(
        "{}",
        crate::text_style::dim(
            "  niu no longer downloads fonts (download retraction 2026-10-04); \
             install one with your package manager, then set it in your terminal"
        )
    );
    println!();
    if nerd_font_installed() {
        println!(
            "  {} a Nerd Font is installed — icon themes are unlocked",
            crate::text_style::green("detected:")
        );
    } else {
        println!(
            "  {} no Nerd Font found — icon themes need one",
            crate::text_style::yellow("missing:")
        );
    }
    println!();
    for font in FONT_OPTIONS {
        println!("  {} ('{}' in your terminal)", font.label, font.face);
        for line in install_lines(font) {
            println!("    {line}");
        }
    }
    println!();
    println!(
        "  {}",
        crate::text_style::dim(
            "after installing: set your terminal font to the face above \
             (Windows Terminal: profile → Appearance → Font face)"
        )
    );
    Ok(())
}

// ── Detection ────────────────────────────────────────────────────────────────

fn font_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(windir) = std::env::var_os("WINDIR") {
        dirs.push(PathBuf::from(windir).join("Fonts"));
    }
    dirs.extend(user_font_dirs());
    dirs
}

#[cfg(windows)]
fn user_font_dirs() -> Vec<PathBuf> {
    user_fonts_dir().into_iter().collect()
}

#[cfg(windows)]
fn user_fonts_dir() -> Option<PathBuf> {
    dirs::data_local_dir().map(|d| d.join("Microsoft").join("Windows").join("Fonts"))
}

/// Per-user font directories on Unix (niubash#607): the classic
/// fontconfig `~/.fonts`, the modern fontconfig home
/// (`$XDG_DATA_HOME/fonts`, defaulting to `~/.local/share/fonts`),
/// and a `$PREFIX/share/fonts` (Termux and other prefix toolchains).
/// Missing directories are no-ops for the scan.
#[cfg(not(windows))]
fn user_font_dirs() -> Vec<PathBuf> {
    unix_font_dirs(
        dirs::home_dir().as_deref(),
        std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .as_deref(),
        std::env::var_os("PREFIX").map(PathBuf::from).as_deref(),
    )
}

/// The pure path math behind [`user_font_dirs`], split out for tests.
#[cfg(not(windows))]
fn unix_font_dirs(
    home: Option<&Path>,
    xdg_data_home: Option<&Path>,
    prefix: Option<&Path>,
) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(home) = home {
        dirs.push(home.join(".fonts"));
        dirs.push(home.join(".local/share/fonts"));
    }
    if let Some(xdg) = xdg_data_home {
        let dir = xdg.join("fonts");
        if !dirs.contains(&dir) {
            dirs.push(dir);
        }
    }
    if let Some(prefix) = prefix {
        dirs.push(prefix.join("share/fonts"));
    }
    dirs
}

/// Termux is a unix (Android) environment; there is no Windows form.
#[cfg(windows)]
fn termux_font_is_nerd_font() -> bool {
    false
}

/// Termux probe (niubash#607): Termux sets the terminal font as one
/// fixed file, `~/.termux/font.ttf`, outside every font directory —
/// and the filename never carries the family name, so directory scans
/// cannot see it. When a Termux signal is present (`TERMUX_VERSION`,
/// or a `$PREFIX` under `com.termux/files/usr`), read that one file
/// and judge it by its name table instead.
#[cfg(not(windows))]
fn termux_font_is_nerd_font() -> bool {
    let Some(home) = dirs::home_dir() else {
        return false;
    };
    termux_font_file_is_nerd(
        std::env::var_os("TERMUX_VERSION").is_some(),
        std::env::var_os("PREFIX").map(PathBuf::from).as_deref(),
        &home,
    )
}

/// The gating behind [`termux_font_is_nerd_font`], split out for tests:
/// without a Termux signal the file is never even read.
#[cfg(not(windows))]
fn termux_font_file_is_nerd(termux_env: bool, prefix: Option<&Path>, home: &Path) -> bool {
    let termux_prefix = prefix
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .is_some_and(|p| p.contains("com.termux/files/usr"));
    if !termux_env && !termux_prefix {
        return false;
    }
    file_name_table_mentions_nerd_font(&home.join(".termux").join("font.ttf"))
}

/// Substring scan of a font file's bytes for the Nerd Font family
/// marker. Name-table strings appear as plain ASCII (Macintosh
/// records) and as UTF-16BE (Windows records); check both shapes.
/// No TTF parser — the Nerd Fonts naming convention always embeds
/// the words `Nerd Font` in the family name.
#[cfg(not(windows))]
fn file_name_table_mentions_nerd_font(path: &Path) -> bool {
    let Ok(bytes) = std::fs::read(path) else {
        return false;
    };
    if bytes.windows(9).any(|w| w == b"Nerd Font") {
        return true;
    }
    let utf16be: Vec<u8> = "Nerd Font".bytes().flat_map(|b| [0, b]).collect();
    bytes.windows(utf16be.len()).any(|w| w == utf16be)
}

fn dir_has_nerd_font(dir: &Path) -> bool {
    std::fs::read_dir(dir)
        .map(|entries| {
            entries.flatten().any(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .to_lowercase()
                    .contains("nerd")
            })
        })
        .unwrap_or(false)
}

/// Value names under `HKCU`/`HKLM ...\Fonts` — the registered font list.
#[cfg(windows)]
fn font_value_names() -> Vec<String> {
    use windows_sys::Win32::Foundation::ERROR_SUCCESS;
    use windows_sys::Win32::System::Registry::{
        RegCloseKey, RegEnumValueW, RegOpenKeyExW, HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE,
        KEY_READ,
    };

    fn to_wide(value: &str) -> Vec<u16> {
        value.encode_utf16().chain(std::iter::once(0)).collect()
    }

    const FONTS_KEY: &str = "SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion\\Fonts";
    let mut names = Vec::new();
    for root in [HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE] {
        let mut key: HKEY = std::ptr::null_mut();
        let opened =
            unsafe { RegOpenKeyExW(root, to_wide(FONTS_KEY).as_ptr(), 0, KEY_READ, &mut key) };
        if opened != ERROR_SUCCESS {
            continue;
        }
        let mut index = 0u32;
        loop {
            let mut buf = vec![0u16; 256];
            let mut len = buf.len() as u32;
            let status = unsafe {
                RegEnumValueW(
                    key,
                    index,
                    buf.as_mut_ptr(),
                    &mut len,
                    std::ptr::null(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                )
            };
            if status != ERROR_SUCCESS {
                break;
            }
            names.push(String::from_utf16_lossy(&buf[..len as usize]));
            index += 1;
        }
        unsafe {
            RegCloseKey(key);
        }
    }
    names
}

#[cfg(windows)]
fn registry_has_nerd_font() -> bool {
    font_value_names()
        .iter()
        .any(|name| name.to_lowercase().contains("nerd"))
}

#[cfg(not(windows))]
fn registry_has_nerd_font() -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn font_options_carry_a_face_and_a_recommendation() {
        for font in FONT_OPTIONS {
            assert!(font.label.contains("Nerd Font"));
            assert!(font.face.contains("Nerd Font"));
            // Every font must recommend at least one channel plus the
            // channel-free fallback (nerdfonts.com is always appended).
            assert!(!install_lines(font).is_empty(), "{}", font.label);
        }
    }

    #[test]
    fn install_lines_are_offline_and_name_the_channels() {
        for font in FONT_OPTIONS {
            let joined = install_lines(font).join("\n");
            assert!(joined.contains("nerdfonts.com"), "{}", font.label);
            if let Some(id) = font.winget_id {
                assert!(
                    joined.contains(&format!("winget install --id {id}")),
                    "{joined}"
                );
            }
        }
    }

    #[test]
    fn menu_labels_cover_options() {
        assert_eq!(menu_labels().len(), FONT_OPTIONS.len());
    }

    #[cfg(not(windows))]
    #[test]
    fn issue607_unix_font_dirs_cover_modern_fontconfig_and_prefix() {
        let home = Path::new("/home/u");
        let dirs = unix_font_dirs(Some(home), None, None);
        assert_eq!(
            dirs,
            vec![
                PathBuf::from("/home/u/.fonts"),
                PathBuf::from("/home/u/.local/share/fonts"),
            ]
        );
        // An explicit XDG_DATA_HOME replaces nothing but adds its fonts
        // dir — unless it is the default, which must not duplicate.
        let dirs = unix_font_dirs(
            Some(home),
            Some(Path::new("/xdg")),
            Some(Path::new("/data/data/com.termux/files/usr")),
        );
        assert_eq!(
            dirs,
            vec![
                PathBuf::from("/home/u/.fonts"),
                PathBuf::from("/home/u/.local/share/fonts"),
                PathBuf::from("/xdg/fonts"),
                PathBuf::from("/data/data/com.termux/files/usr/share/fonts"),
            ]
        );
        let dirs = unix_font_dirs(Some(home), Some(Path::new("/home/u/.local/share")), None);
        assert_eq!(dirs.len(), 2);
    }

    #[cfg(not(windows))]
    #[test]
    fn issue607_termux_font_is_judged_by_name_table_not_filename() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let termux_dir = home.join(".termux");
        std::fs::create_dir_all(&termux_dir).unwrap();
        // A realistic shape: binary padding around an ASCII name-table
        // family string. The filename is font.ttf — no "nerd" in it.
        let mut bytes = vec![0u8; 64];
        bytes.extend_from_slice(b"Hack Nerd Font Mono Regular");
        bytes.extend_from_slice(&[0u8; 16]);
        std::fs::write(termux_dir.join("font.ttf"), &bytes).unwrap();

        // Gated off without any Termux signal: the file is not read.
        assert!(!termux_font_file_is_nerd(false, None, home));
        assert!(!termux_font_file_is_nerd(
            false,
            Some(Path::new("/usr")),
            home
        ));
        // Either signal opens the probe.
        assert!(termux_font_file_is_nerd(true, None, home));
        assert!(termux_font_file_is_nerd(
            false,
            Some(Path::new("/data/data/com.termux/files/usr")),
            home
        ));

        // A non-Nerd family in the same file shape does not detect.
        let mut bytes = vec![0u8; 32];
        bytes.extend_from_slice(b"Hack Mono Regular");
        std::fs::write(termux_dir.join("font.ttf"), &bytes).unwrap();
        assert!(!termux_font_file_is_nerd(true, None, home));

        // UTF-16BE name-table records (Windows platform) also detect.
        let mut bytes = vec![0u8; 8];
        bytes.extend(
            "FiraCode Nerd Font"
                .encode_utf16()
                .flat_map(|u| u.to_be_bytes()),
        );
        std::fs::write(termux_dir.join("font.ttf"), &bytes).unwrap();
        assert!(termux_font_file_is_nerd(true, None, home));
    }
}
