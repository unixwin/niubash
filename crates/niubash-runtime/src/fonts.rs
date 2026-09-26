//! Nerd Font detection, download, and per-user installation.
//!
//! Fonts install to `%LOCALAPPDATA%\Microsoft\Windows\Fonts` and register
//! under `HKCU\...\Fonts`, so no administrator rights are needed on
//! Windows 10 1809+. Downloads come from the nerd-fonts GitHub release
//! assets via the system `curl.exe`. On Unix, TTFs are extracted into
//! `~/.fonts` (fontconfig scans it without any registration step) and
//! downloads use the system `curl` from PATH.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{bail, Context, Result};

const NERD_FONTS_RELEASE: &str = "https://github.com/ryanoasis/nerd-fonts/releases/latest/download";

/// A selectable Nerd Font from the nerd-fonts release catalog.
pub struct NerdFont {
    /// Menu label shown to the user.
    pub label: &'static str,
    /// Value written to a Windows Terminal profile's `font.face`.
    pub face: &'static str,
    /// Release asset name, e.g. `JetBrainsMono.zip`.
    pub asset: &'static str,
    /// Substring selecting the mono-spaced family TTFs inside the zip.
    pub file_marker: &'static str,
}

/// Fonts offered by the setup wizard and `niu font`.
pub const FONT_OPTIONS: &[NerdFont] = &[
    NerdFont {
        label: "JetBrainsMono Nerd Font",
        face: "JetBrainsMono Nerd Font Mono",
        asset: "JetBrainsMono.zip",
        file_marker: "NerdFontMono-",
    },
    NerdFont {
        label: "MesloLGM Nerd Font",
        face: "MesloLGM Nerd Font Mono",
        asset: "Meslo.zip",
        file_marker: "LGMNerdFontMono-",
    },
    NerdFont {
        label: "CaskaydiaCove Nerd Font",
        face: "CaskaydiaCove Nerd Font Mono",
        asset: "CascadiaCode.zip",
        file_marker: "CoveNerdFontMono-",
    },
];

/// Result of a successful font install.
pub struct InstalledFont {
    /// The `font.face` name terminals should use.
    pub face: &'static str,
    /// Number of TTF files installed.
    pub files: usize,
    /// Directory the fonts were copied into.
    pub dir: PathBuf,
}

/// Menu labels for the font choice question (plus room for a Skip entry
/// appended by the caller).
pub fn menu_labels() -> Vec<String> {
    FONT_OPTIONS.iter().map(|f| f.label.to_string()).collect()
}

/// True when any installed font name or file looks like a Nerd Font.
/// Checks the per-user and system font directories plus the registry
/// font lists (HKCU and HKLM).
pub fn nerd_font_installed() -> bool {
    font_dirs().iter().any(|dir| dir_has_nerd_font(dir)) || registry_has_nerd_font()
}

/// Download `font`, install its mono family into the per-user font
/// directory, register each face, and broadcast `WM_FONTCHANGE`.
pub fn install(font: &NerdFont) -> Result<InstalledFont> {
    let fonts_dir = user_fonts_dir().context("locate per-user fonts directory")?;
    std::fs::create_dir_all(&fonts_dir)
        .with_context(|| format!("create {}", fonts_dir.display()))?;

    let zip_path = std::env::temp_dir().join(format!("niu-font-{}", font.asset));
    download(&format!("{NERD_FONTS_RELEASE}/{}", font.asset), &zip_path)?;
    let installed = extract_family(&zip_path, &fonts_dir, font.file_marker)?;
    let _ = std::fs::remove_file(&zip_path);
    if installed.is_empty() {
        bail!("no matching font files in {}", font.asset);
    }

    for path in &installed {
        register_font(path);
    }
    broadcast_font_change();

    Ok(InstalledFont {
        face: font.face,
        files: installed.len(),
        dir: fonts_dir,
    })
}

/// `niu font` — pick a Nerd Font, install it per-user, and point the
/// Windows Terminal Niubash profile at it when one exists.
pub fn run_font_command() -> Result<()> {
    use crate::interactive_menu::{interactive_choice, Selection};

    if !crate::terminal::stdio_is_interactive() {
        bail!("`niu font` needs an interactive terminal");
    }
    if nerd_font_installed() {
        println!("A Nerd Font is already installed — installing another is fine.");
    }
    let mut labels = menu_labels();
    labels.push("Cancel".to_string());
    let refs: Vec<&str> = labels.iter().map(String::as_str).collect();
    let selection = interactive_choice(
        "Choose a Nerd Font to install",
        &refs,
        0,
        "installs to your user fonts folder — no admin needed",
    );
    let Selection::Confirmed(idx) = selection else {
        println!("Cancelled — nothing was installed.");
        return Ok(());
    };
    if idx >= FONT_OPTIONS.len() {
        return Ok(());
    }
    let installed = install(&FONT_OPTIONS[idx])?;
    println!(
        "Installed {} ({} files) to {}",
        FONT_OPTIONS[idx].label,
        installed.files,
        installed.dir.display()
    );
    if std::env::var_os("WT_SESSION").is_some() {
        if wt_profile_font_set(installed.face) {
            println!(
                "Windows Terminal Niubash profile now uses '{}'.",
                installed.face
            );
        } else {
            println!(
                "Set your terminal font to '{}' (Windows Terminal: profile → Appearance → Font face).",
                installed.face
            );
        }
    } else {
        println!("Now set your terminal font to '{}'.", installed.face);
    }
    Ok(())
}

/// True when the Windows Terminal Niubash profile was pointed at `face`.
/// Windows Terminal is Windows-only; elsewhere this is always false so the
/// caller prints the manual "set your font" hint instead.
#[cfg(windows)]
fn wt_profile_font_set(face: &str) -> bool {
    matches!(
        crate::windows_terminal::set_niubash_profile_font(face),
        Ok(summary) if !summary.updated.is_empty()
    )
}

#[cfg(not(windows))]
fn wt_profile_font_set(_face: &str) -> bool {
    false
}

// ── Download & extract ───────────────────────────────────────────────────────

/// Locate the download helper: `curl.exe` — System32 on Windows 10 1803+,
/// else PATH. On Unix, the system `curl` from PATH.
#[cfg(windows)]
fn curl_command() -> Command {
    let sys32 = std::env::var_os("WINDIR")
        .map(PathBuf::from)
        .map(|w| w.join("System32").join("curl.exe"));
    match sys32 {
        Some(path) if path.is_file() => Command::new(path),
        _ => Command::new("curl.exe"),
    }
}

#[cfg(not(windows))]
fn curl_command() -> Command {
    Command::new("curl")
}

fn download(url: &str, dest: &Path) -> Result<()> {
    let status = curl_command()
        .arg("-fSL")
        .arg("--retry")
        .arg("2")
        .arg("-o")
        .arg(dest)
        .arg(url)
        .stdin(Stdio::null())
        .status()
        .context("run curl.exe")?;
    if !status.success() {
        bail!("download failed (curl exited with {status}): {url}");
    }
    Ok(())
}

/// Extract TTFs whose file name contains `marker` into `dest`; returns the
/// written paths.
fn extract_family(zip_path: &Path, dest: &Path, marker: &str) -> Result<Vec<PathBuf>> {
    let file =
        std::fs::File::open(zip_path).with_context(|| format!("open {}", zip_path.display()))?;
    let mut archive = zip::ZipArchive::new(file).context("read font zip")?;
    let mut written = Vec::new();
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i)?;
        let Some(name) = entry.enclosed_name().map(|n| n.to_path_buf()) else {
            continue;
        };
        let is_ttf = name
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("ttf"));
        let file_name = name
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default()
            .to_string();
        if !is_ttf || !file_name.contains(marker) {
            continue;
        }
        let target = dest.join(&file_name);
        let mut out = std::fs::File::create(&target)
            .with_context(|| format!("create {}", target.display()))?;
        std::io::copy(&mut entry, &mut out)
            .with_context(|| format!("write {}", target.display()))?;
        written.push(target);
    }
    Ok(written)
}

// ── Detection ────────────────────────────────────────────────────────────────

fn font_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(windir) = std::env::var_os("WINDIR") {
        dirs.push(PathBuf::from(windir).join("Fonts"));
    }
    if let Some(local) = user_fonts_dir() {
        dirs.push(local);
    }
    dirs
}

#[cfg(windows)]
fn user_fonts_dir() -> Option<PathBuf> {
    dirs::data_local_dir().map(|d| d.join("Microsoft").join("Windows").join("Fonts"))
}

/// Per-user font directory on Unix: the classic fontconfig `~/.fonts`,
/// which desktop environments scan without any registration step.
#[cfg(not(windows))]
fn user_fonts_dir() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".fonts"))
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

// ── Windows font registration ────────────────────────────────────────────────

#[cfg(windows)]
fn to_wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Register one installed TTF for the current user: registry entry plus
/// `AddFontResourceW` so the face works in this session too.
#[cfg(windows)]
fn register_font(path: &Path) {
    use windows_sys::Win32::Graphics::Gdi::AddFontResourceW;

    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("NiubashFont");
    let value_name = format!("{stem} (TrueType)");
    set_user_font_value(&value_name, &path.to_string_lossy());
    unsafe {
        AddFontResourceW(to_wide(&path.to_string_lossy()).as_ptr());
    }
}

/// Tell running apps the font list changed.
#[cfg(windows)]
fn broadcast_font_change() {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        PostMessageW, HWND_BROADCAST, WM_FONTCHANGE,
    };
    unsafe {
        PostMessageW(HWND_BROADCAST, WM_FONTCHANGE, 0, 0);
    }
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

#[cfg(not(windows))]
fn register_font(_path: &Path) {}

#[cfg(not(windows))]
fn broadcast_font_change() {}

/// Value names under `HKCU`/`HKLM ...\Fonts` — the registered font list.
#[cfg(windows)]
fn font_value_names() -> Vec<String> {
    use windows_sys::Win32::Foundation::ERROR_SUCCESS;
    use windows_sys::Win32::System::Registry::{
        RegCloseKey, RegEnumValueW, RegOpenKeyExW, HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE,
        KEY_READ,
    };

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

/// Write `name = <ttf path>` into the per-user Fonts registry key so the
/// font persists across logons.
#[cfg(windows)]
fn set_user_font_value(name: &str, path: &str) {
    use windows_sys::Win32::Foundation::ERROR_SUCCESS;
    use windows_sys::Win32::System::Registry::{
        RegCloseKey, RegCreateKeyExW, RegSetValueExW, HKEY, HKEY_CURRENT_USER, KEY_WRITE, REG_SZ,
    };

    const FONTS_KEY: &str = "SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion\\Fonts";
    let mut key: HKEY = std::ptr::null_mut();
    let created = unsafe {
        RegCreateKeyExW(
            HKEY_CURRENT_USER,
            to_wide(FONTS_KEY).as_ptr(),
            0,
            std::ptr::null(),
            0,
            KEY_WRITE,
            std::ptr::null(),
            &mut key,
            std::ptr::null_mut(),
        )
    };
    if created != ERROR_SUCCESS {
        return;
    }
    let data = to_wide(path);
    unsafe {
        RegSetValueExW(
            key,
            to_wide(name).as_ptr(),
            0,
            REG_SZ,
            data.as_ptr() as *const u8,
            (data.len() * 2) as u32,
        );
        RegCloseKey(key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn font_options_have_matching_markers() {
        for font in FONT_OPTIONS {
            assert!(font.label.contains("Nerd Font"));
            assert!(font.face.contains("Nerd Font"));
            assert!(font.asset.ends_with(".zip"));
            assert!(font.file_marker.contains("NerdFontMono-"));
        }
    }

    #[test]
    fn menu_labels_cover_options() {
        assert_eq!(menu_labels().len(), FONT_OPTIONS.len());
    }
}
