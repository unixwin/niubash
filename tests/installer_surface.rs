//! Static guard over the installer surface (`installer/niubash.iss`).
//!
//! niubash#398 found an unowned .NET shim occupying the
//! `C:\Program Files\Git\bin\bash.exe` convention slot and suspected the niu
//! installer had put it there. The investigation (PR #414) overturned the
//! premise — the installer runs with `PrivilegesRequired=lowest`, never
//! elevates, and the slot directory is admin-walled — and ruled that the
//! install tree does NOT claim the slot. That ruling lived only in a GitHub
//! comment; these tests pin it to the repository so the surface cannot drift
//! back: every write, delete and run surface of the installer stays inside
//! the app-owned tree (plus the one HKCU PATH value), and nothing in the
//! script can even name a foreign absolute location.
//!
//! This is the "installer script logic review" leg of the #398 verification;
//! `scripts/installer-slot-probe.py` is the runtime leg (controlled-directory
//! install/relink/uninstall replay with byte-identity checks).

use std::fs;
use std::path::PathBuf;

fn installer_script() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("installer")
        .join("niubash.iss")
}

/// The whole script, lines attached, for failure messages that point at the
/// offending line the way a review would.
fn script_lines() -> Vec<(usize, String)> {
    let text = fs::read_to_string(installer_script())
        .expect("installer/niubash.iss must exist next to the crate");
    text.lines()
        .enumerate()
        .map(|(i, line)| (i + 1, line.to_string()))
        .collect()
}

fn lines_matching(lines: &[(usize, String)], needle: &str) -> Vec<(usize, String)> {
    lines
        .iter()
        .filter(|(_, line)| !line.trim_start().starts_with(';') && line.contains(needle))
        .cloned()
        .collect()
}

/// Non-comment `[Section]` headers present in `lines`, lowercased without brackets.
fn section_headers(lines: &[(usize, String)]) -> Vec<(usize, String)> {
    lines
        .iter()
        .filter_map(|(number, line)| {
            let trimmed = line.trim();
            if trimmed.starts_with('[') && trimmed.ends_with(']') {
                Some((*number, trimmed[1..trimmed.len() - 1].trim().to_lowercase()))
            } else {
                None
            }
        })
        .collect()
}

/// Expand Inno Setup's directory constants into their canonical absolute
/// paths so constant-folded spellings like `{autopf}\Git` or
/// `{syswow64}\cmd.exe` cannot sneak past the literal drive-path and
/// `Program Files` scanners (niubash#442). Every constant that resolves
/// outside the sanctioned surface folds here — the exceptions are the
/// sanctioned locations themselves ({app}, {localappdata}, the icon folders
/// {group}/{autodesktop}/{userdesktop}, {%USERPROFILE}) and {sys}, which
/// stays unfolded on purpose: the single sanctioned {sys} binary
/// ({sys}\taskkill.exe) is owned by the run/binary allow-list below.
fn expand_foreign_inno_constants(line: &str) -> String {
    let mut out = line.to_string();
    for (constant, expansion) in [
        // Program Files — administrative, auto and per-user spellings.
        ("{autopf}", "C:\\Program Files"),
        ("{autopf32}", "C:\\Program Files (x86)"),
        ("{autopf64}", "C:\\Program Files"),
        ("{commonpf}", "C:\\Program Files"),
        ("{commonpf32}", "C:\\Program Files (x86)"),
        ("{commonpf64}", "C:\\Program Files"),
        ("{pf}", "C:\\Program Files"),
        ("{pf32}", "C:\\Program Files (x86)"),
        ("{pf64}", "C:\\Program Files"),
        ("{userpf}", "C:\\Users\\user\\AppData\\Local\\Programs"),
        // Common Files.
        ("{autocf}", "C:\\Program Files\\Common Files"),
        ("{autocf32}", "C:\\Program Files (x86)\\Common Files"),
        ("{autocf64}", "C:\\Program Files\\Common Files"),
        ("{commoncf}", "C:\\Program Files\\Common Files"),
        ("{commoncf32}", "C:\\Program Files (x86)\\Common Files"),
        ("{commoncf64}", "C:\\Program Files\\Common Files"),
        ("{cf}", "C:\\Program Files\\Common Files"),
        ("{cf32}", "C:\\Program Files (x86)\\Common Files"),
        ("{cf64}", "C:\\Program Files\\Common Files"),
        (
            "{usercf}",
            "C:\\Users\\user\\AppData\\Local\\Programs\\Common",
        ),
        (
            "{dao}",
            "C:\\Program Files\\Common Files\\Microsoft Shared\\DAO",
        ),
        // Windows and system directories.
        ("{sd}", "C:"),
        ("{win}", "C:\\Windows"),
        ("{sysnative}", "C:\\Windows\\System32"),
        ("{syswow64}", "C:\\Windows\\SysWOW64"),
        ("{cmd}", "C:\\Windows\\System32\\cmd.exe"),
        ("{commonfonts}", "C:\\Windows\\Fonts"),
        ("{fonts}", "C:\\Windows\\Fonts"),
        ("{dotnet11}", "C:\\Windows\\Microsoft.NET"),
        ("{dotnet20}", "C:\\Windows\\Microsoft.NET"),
        ("{dotnet2032}", "C:\\Windows\\Microsoft.NET"),
        ("{dotnet2064}", "C:\\Windows\\Microsoft.NET"),
        ("{dotnet40}", "C:\\Windows\\Microsoft.NET"),
        ("{dotnet4032}", "C:\\Windows\\Microsoft.NET"),
        ("{dotnet4064}", "C:\\Windows\\Microsoft.NET"),
        // Application data / all-users profile roots.
        ("{commonappdata}", "C:\\ProgramData"),
        ("{autoappdata}", "C:\\ProgramData"),
        ("{userappdata}", "C:\\Users\\user\\AppData\\Roaming"),
        // Shell folders outside the sanctioned icon set.
        ("{commondesktop}", "C:\\Users\\Public\\Desktop"),
        ("{userdocs}", "C:\\Users\\user\\Documents"),
        ("{commondocs}", "C:\\Users\\Public\\Documents"),
        ("{autodocs}", "C:\\Users\\user\\Documents"),
        ("{userfavorites}", "C:\\Users\\user\\Favorites"),
        (
            "{userfonts}",
            "C:\\Users\\user\\AppData\\Local\\Microsoft\\Windows\\Fonts",
        ),
        ("{autofonts}", "C:\\Windows\\Fonts"),
        (
            "{userprograms}",
            "C:\\Users\\user\\AppData\\Roaming\\Microsoft\\Windows\\Start Menu\\Programs",
        ),
        (
            "{commonprograms}",
            "C:\\ProgramData\\Microsoft\\Windows\\Start Menu\\Programs",
        ),
        (
            "{autoprograms}",
            "C:\\ProgramData\\Microsoft\\Windows\\Start Menu\\Programs",
        ),
        (
            "{userstartmenu}",
            "C:\\Users\\user\\AppData\\Roaming\\Microsoft\\Windows\\Start Menu",
        ),
        (
            "{commonstartmenu}",
            "C:\\ProgramData\\Microsoft\\Windows\\Start Menu",
        ),
        (
            "{autostartmenu}",
            "C:\\ProgramData\\Microsoft\\Windows\\Start Menu",
        ),
        (
            "{userstartup}",
            "C:\\Users\\user\\AppData\\Roaming\\Microsoft\\Windows\\Start Menu\\Programs\\Startup",
        ),
        (
            "{commonstartup}",
            "C:\\ProgramData\\Microsoft\\Windows\\Start Menu\\Programs\\Startup",
        ),
        (
            "{autostartup}",
            "C:\\ProgramData\\Microsoft\\Windows\\Start Menu\\Programs\\Startup",
        ),
        (
            "{usersendto}",
            "C:\\Users\\user\\AppData\\Roaming\\Microsoft\\Windows\\SendTo",
        ),
        (
            "{sendto}",
            "C:\\Users\\user\\AppData\\Roaming\\Microsoft\\Windows\\SendTo",
        ),
        (
            "{usertemplates}",
            "C:\\Users\\user\\AppData\\Roaming\\Microsoft\\Windows\\Templates",
        ),
        (
            "{commontemplates}",
            "C:\\ProgramData\\Microsoft\\Windows\\Templates",
        ),
        (
            "{autotemplates}",
            "C:\\ProgramData\\Microsoft\\Windows\\Templates",
        ),
        ("{usersavedgames}", "C:\\Users\\user\\Saved Games"),
    ] {
        // Case-insensitive replacement for Inno constants.
        let lower = out.to_lowercase();
        if lower.contains(constant) {
            let mut rebuilt = String::with_capacity(out.len() + 16);
            let mut rest = out.as_str();
            while let Some(pos) = rest.to_lowercase().find(constant) {
                rebuilt.push_str(&rest[..pos]);
                rebuilt.push_str(expansion);
                rest = &rest[pos + constant.len()..];
            }
            rebuilt.push_str(rest);
            out = rebuilt;
        }
    }
    out
}

/// Extract `Key: value` entries from a named section. Inno directive lines
/// are `Key: "value"; Key: "value"; ...` — split each non-comment line on
/// the `;` separators and return the quoted-or-bare `value` for every piece
/// whose key matches `keyword`. Section bodies run until the next `[Bracket]`
/// header. (None of the directives in this script carry a `;` inside a quoted
/// value; if one ever does, this parser must grow quote awareness first.)
fn section_entries(
    lines: &[(usize, String)],
    section: &str,
    keyword: &str,
) -> Vec<(usize, String)> {
    let header = format!("[{section}]");
    let mut in_section = false;
    let mut found = Vec::new();
    let prefix = format!("{keyword}:");
    for (number, line) in lines {
        let trimmed = line.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            in_section = trimmed == header;
            continue;
        }
        if !in_section || trimmed.starts_with(';') || trimmed.is_empty() {
            continue;
        }
        for piece in trimmed.split(';') {
            let piece = piece.trim();
            if let Some(rest) = piece.strip_prefix(&prefix) {
                let value = rest.trim().trim_matches('"').to_string();
                found.push((*number, value));
            }
        }
    }
    found
}

/// niubash#398 ruling, fact 1: the installer never elevates, so it can never
/// write an admin-walled foreign slot such as `C:\Program Files\Git\bin`.
#[test]
fn installer_requires_lowest_privileges() {
    let lines = script_lines();
    assert!(
        check_lowest_privileges(&lines).is_ok(),
        "{}",
        check_lowest_privileges(&lines).unwrap_err()
    );
}

fn check_lowest_privileges(lines: &[(usize, String)]) -> Result<(), String> {
    let declared = lines_matching(lines, "PrivilegesRequired");
    if declared.len() != 1 {
        return Err(format!(
            "exactly one non-comment PrivilegesRequired line expected: {declared:?}"
        ));
    }
    let normalized = declared[0].1.replace(' ', "").to_lowercase();
    if normalized != "privilegesrequired=lowest" {
        return Err(format!(
            "PrivilegesRequired must stay `lowest` — elevation is the only way this \
             installer could ever touch a foreign slot like C:\\Program Files\\Git\\bin, \
             and niubash#398's ruling is that it never does: {declared:?}"
        ));
    }
    Ok(())
}

/// niubash#442: `[Registry]`, `[INI]`, and `[UninstallRun]` sections are
/// forbidden (fail-closed). The one sanctioned HKCU PATH update lives in
/// `[Code]` (`registry_writes_confine_to_the_user_environment_path_value`);
/// opening any of these declarative sections would create an unguarded write
/// or execution surface.
#[test]
fn installer_forbids_unguarded_sections() {
    let lines = script_lines();
    assert!(
        check_forbidden_sections(&lines).is_ok(),
        "{}",
        check_forbidden_sections(&lines).unwrap_err()
    );
}

fn check_forbidden_sections(lines: &[(usize, String)]) -> Result<(), String> {
    const FORBIDDEN_SECTIONS: [&str; 3] = ["registry", "ini", "uninstallrun"];
    let mut offenders = Vec::new();
    for (number, name) in section_headers(lines) {
        if FORBIDDEN_SECTIONS.contains(&name.as_str()) {
            offenders.push(format!("line {number}: forbidden section [{name}]"));
        }
    }
    if offenders.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "installer/niubash.iss must not declare [Registry], [INI], or [UninstallRun] \
             sections (niubash#442 fail-closed guard):\n{}",
            offenders.join("\n")
        ))
    }
}

/// niubash#398 ruling, fact 2: the script's world is Inno constants and
/// relative names. No absolute drive path and no `Program Files` reference
/// may appear anywhere — including comments and constant-folded `{autopf}` /
/// `{pf}` spellings (niubash#442) — so the Git Bash slot cannot be named, let
/// alone written, from this surface.
#[test]
fn installer_never_names_an_absolute_foreign_location() {
    let lines = script_lines();
    assert!(
        check_no_foreign_locations(&lines).is_ok(),
        "{}",
        check_no_foreign_locations(&lines).unwrap_err()
    );
}

fn check_no_foreign_locations(lines: &[(usize, String)]) -> Result<(), String> {
    let mut offenders = Vec::new();
    for (number, raw_line) in lines {
        let expanded = expand_foreign_inno_constants(raw_line);
        // A drive-letter absolute path: `X:` followed by `\` or `/`, where
        // the letter starts a word (this excludes URL schemes like `https:`).
        let bytes = expanded.as_bytes();
        let mut index = 0;
        while index + 2 < bytes.len() {
            let starts_word = index == 0 || !bytes[index - 1].is_ascii_alphabetic();
            if starts_word
                && bytes[index].is_ascii_alphabetic()
                && bytes[index + 1] == b':'
                && (bytes[index + 2] == b'\\' || bytes[index + 2] == b'/')
            {
                offenders.push(format!("line {number}: absolute path — {raw_line:?}"));
                break;
            }
            index += 1;
        }
        if expanded.to_lowercase().contains("program files") {
            offenders.push(format!(
                "line {number}: 'Program Files' reference — {raw_line:?}"
            ));
        }
    }
    if offenders.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "installer/niubash.iss must not reference any absolute foreign location \
             (niubash#398/#442: the Git Bash slot stays unclaimed; the app tree is reached \
             through Inno constants like {{app}}/{{localappdata}}):\n{}",
            offenders.join("\n")
        ))
    }
}

/// Every file the installer writes or deletes lives under `{app}` (or is an
/// exact retired name under `{app}`), so an uninstall can never leave a
/// dangling artifact in a directory niu does not own.
#[test]
fn file_write_and_delete_surfaces_stay_under_the_app_directory() {
    let lines = script_lines();

    for section in ["Files", "InstallDelete", "UninstallDelete"] {
        let keyword = if section == "Files" {
            "DestDir"
        } else {
            "Name"
        };
        for (number, value) in section_entries(&lines, section, keyword) {
            let bare = value.trim_start_matches('"').trim_end_matches('"');
            assert!(
                bare == "{app}" || bare.starts_with("{app}\\"),
                "[{section}] line {number} writes outside the app tree: {value:?} — \
                 niubash#398: the installer's filesystem surface is {{app}}-confined"
            );
        }
    }
}

/// Inno wildcard match, `*` = any run, case-insensitive — enough for the
/// sanctioned retired-shape patterns (each carries at most two stars).
fn glob_matches(pattern: &str, text: &str) -> bool {
    let pattern = pattern.to_lowercase();
    let text = text.to_lowercase();
    let parts: Vec<&str> = pattern.split('*').collect();
    if parts.len() == 1 {
        return pattern == text;
    }
    let mut cursor = 0;
    for (index, part) in parts.iter().enumerate() {
        if index == 0 {
            if !text.starts_with(part) {
                return false;
            }
            cursor = part.len();
        } else if index == parts.len() - 1 {
            // The final literal must close the string, past everything found
            // so far.
            if text.len() < cursor + part.len() || !text[cursor..].ends_with(part) {
                return false;
            }
        } else {
            match text[cursor..].find(part) {
                Some(position) => cursor += position + part.len(),
                None => return false,
            }
        }
    }
    true
}

/// The delete entries only ever match generated retired-shape wildcards (plus
/// the one exact retired binary name), never a live entry file. This is what
/// keeps the #376 sweep from eating a user's `bash.exe`/`sh.exe`/`git.exe`
/// while it cleans the dead ones — and what makes the uninstall log replay
/// unable to reach outside {app} even through a wildcard.
#[test]
fn delete_entries_match_only_retired_shapes() {
    let lines = script_lines();
    // Shapes the [InstallDelete] comment sanctions: generated backup shapes
    // only. An exact name must be a retired binary (allow-listed here).
    const SANCTIONED_SHAPES: [&str; 3] = ["*.bak-*", "*.old-*", "*.niu-old-*"];
    const RETIRED_EXACT: [&str; 1] = ["{app}\\niubash.exe"];

    for section in ["InstallDelete", "UninstallDelete"] {
        for (number, value) in section_entries(&lines, section, "Name") {
            let bare = value.trim_start_matches('"').trim_end_matches('"');
            if RETIRED_EXACT
                .iter()
                .any(|allowed| allowed.eq_ignore_ascii_case(bare))
            {
                continue;
            }
            assert!(
                SANCTIONED_SHAPES
                    .iter()
                    .any(|shape| glob_matches(shape, bare)),
                "[{section}] line {number} deletes by a non-retired-shape name: {value:?} — \
                 sweeps must match generated shapes ({SANCTIONED_SHAPES:?}) or an \
                 allow-listed retired name, never a live entry"
            );
        }
    }
}

/// Every external process the installer launches comes from the app tree, so
/// the [Run] steps execute the payload niu.exe (the #397 relink) and the
/// bundled winuxcmd dispatcher — never a downloaded or foreign binary.
/// Across the entire script, `{sys}\taskkill.exe` is the ONLY sanctioned
/// `{sys}` binary (niubash#442).
#[test]
fn run_steps_execute_only_app_tree_binaries() {
    let lines = script_lines();
    assert!(
        check_run_and_sys_binaries(&lines).is_ok(),
        "{}",
        check_run_and_sys_binaries(&lines).unwrap_err()
    );
}

fn check_run_and_sys_binaries(lines: &[(usize, String)]) -> Result<(), String> {
    for (number, value) in section_entries(lines, "Run", "Filename") {
        let bare = value.trim_start_matches('"').trim_end_matches('"');
        if !bare.starts_with("{app}\\") && !bare.eq_ignore_ascii_case("{sys}\\taskkill.exe") {
            return Err(format!(
                "[Run] line {number} launches a binary from outside the app tree: {value:?} — \
                 only {{app}} payloads (and {{sys}}\\taskkill.exe for the silent-update \
                 close) may run (niubash#398/#442: nothing reaches for a foreign slot)"
            ));
        }
    }
    const SANCTIONED_SYS_BINARY: &str = "{sys}\\taskkill.exe";
    for (number, line) in lines {
        if line.trim_start().starts_with(';') {
            continue;
        }
        let lower = line.to_lowercase();
        let mut rest = lower.as_str();
        while let Some(pos) = rest.find("{sys}") {
            let tail = &rest[pos..];
            if !tail.starts_with(SANCTIONED_SYS_BINARY) {
                return Err(format!(
                    "line {number}: unsanctioned {{sys}} binary reference (only {SANCTIONED_SYS_BINARY} is allowed): {line:?}"
                ));
            }
            // The sanction ends at the binary name — `{sys}\taskkill.exe2`
            // or `{sys}\taskkill.exe.old` is a different file, not taskkill.
            let after = &tail[SANCTIONED_SYS_BINARY.len()..];
            match after.chars().next() {
                Some(c) if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') => {
                    return Err(format!(
                        "line {number}: {{sys}} reference continues past the sanctioned \
                         binary name (only {SANCTIONED_SYS_BINARY} is allowed): {line:?}"
                    ));
                }
                _ => rest = after,
            }
        }
    }
    Ok(())
}

/// The only registry surface is the user `Environment\Path` value: add on
/// install (the PATH task), sweep on uninstall. Anything else — an uninstall
/// that wrote a marker, a repair that poked a foreign registration — would be
/// a #398-shaped surprise: state left behind that outlives the app tree.
#[test]
fn registry_writes_confine_to_the_user_environment_path_value() {
    let lines = script_lines();
    let writes = lines_matching(&lines, "RegWrite");
    assert!(
        !writes.is_empty(),
        "the PATH task requires its registry writes; found none — did the \
         mechanism move? Update this guard with it deliberately"
    );
    for (number, line) in &writes {
        assert!(
            line.contains("HKEY_CURRENT_USER"),
            "line {number}: registry write outside HKEY_CURRENT_USER: {line:?}"
        );
        assert!(
            line.contains("NiubashEnvironmentKey") && line.contains("NiubashPathValueName"),
            "line {number}: registry write outside the Environment\\Path value: {line:?}"
        );
    }
    // The constants those writes go through pin the exact key and value.
    let key_line = lines_matching(&lines, "NiubashEnvironmentKey =");
    let value_line = lines_matching(&lines, "NiubashPathValueName =");
    assert!(
        key_line.iter().any(|(_, l)| l.contains("'Environment'")),
        "NiubashEnvironmentKey must name the HKCU 'Environment' key: {key_line:?}"
    );
    assert!(
        value_line.iter().any(|(_, l)| l.contains("'Path'")),
        "NiubashPathValueName must name the 'Path' value: {value_line:?}"
    );
}

/// Shortcuts point at the payload in {app}; their locations are Inno-managed
/// shell folders that the uninstall log owns, so no shortcut can dangle a
/// foreign slot into a session after uninstall (the #398 failure shape).
#[test]
fn shortcuts_point_into_the_app_tree() {
    let lines = script_lines();
    const SHELL_FOLDER_PREFIXES: [&str; 3] = ["{group}", "{autodesktop}", "{userdesktop}"];
    for (number, value) in section_entries(&lines, "Icons", "Filename") {
        let bare = value.trim_start_matches('"').trim_end_matches('"');
        assert!(
            bare.starts_with("{app}\\"),
            "[Icons] line {number}: shortcut target outside the app tree: {value:?}"
        );
    }
    for (number, value) in section_entries(&lines, "Icons", "Name") {
        let bare = value.trim_start_matches('"').trim_end_matches('"');
        assert!(
            SHELL_FOLDER_PREFIXES
                .iter()
                .any(|prefix| bare.starts_with(prefix)),
            "[Icons] line {number}: shortcut placed outside Inno-managed shell \
             folders {SHELL_FOLDER_PREFIXES:?}: {value:?}"
        );
    }
}

/// niubash#442 adversarial mutation suite: verify that the CR mutation
/// vectors against `installer/niubash.iss` are caught by the guards above —
/// and that each rejection names the surface that tripped it (the ticket's
/// "guard failure message corresponds to the triggering section" clause).
/// Vectors 8–10 extend the suite to the spelling variants the same
/// experiment shape permits: the {syswow64} alias for the system directory,
/// a suffix that continues past the sanctioned taskkill.exe name, and a
/// non-Program-Files foreign root ({commonappdata}).
#[test]
fn cr_adversarial_mutations_are_rejected() {
    let base = script_lines();

    // 1. [Registry] section writing HKLM — the declarative form is invisible
    // to the [Code] RegWrite grep, so the fail-closed section ban must fire.
    let mut m1 = base.clone();
    m1.push((999, "[Registry]".to_string()));
    m1.push((
        1000,
        r#"Root: HKLM; Subkey: "Software\Git"; ValueType: string; ValueData: "{app}""#.to_string(),
    ));
    let err = check_forbidden_sections(&m1).unwrap_err();
    assert!(
        err.contains("[registry]"),
        "rejection must name the forbidden section: {err}"
    );

    // 2. [INI] section writing {autopf}
    let mut m2 = base.clone();
    m2.push((999, "[INI]".to_string()));
    m2.push((
        1000,
        r#"Filename: "{autopf}\Git\etc\gitconfig""#.to_string(),
    ));
    let err = check_forbidden_sections(&m2).unwrap_err();
    assert!(
        err.contains("[ini]"),
        "rejection must name the forbidden section: {err}"
    );
    let err = check_no_foreign_locations(&m2).unwrap_err();
    assert!(
        err.contains("Program Files"),
        "folded {{autopf}} must surface as a foreign location: {err}"
    );

    // 3. [UninstallRun] + {autopf}
    let mut m3 = base.clone();
    m3.push((999, "[UninstallRun]".to_string()));
    m3.push((1000, r#"Filename: "{autopf}\Git\unins000.exe""#.to_string()));
    let err = check_forbidden_sections(&m3).unwrap_err();
    assert!(
        err.contains("[uninstallrun]"),
        "rejection must name the forbidden section: {err}"
    );
    assert!(
        check_no_foreign_locations(&m3)
            .unwrap_err()
            .contains("Program Files"),
        "folded {{autopf}} must surface as a foreign location"
    );

    // 4. {autopf} constant folding bypassing literal drive / Program Files
    // scan — expansion must expose both signatures.
    let mut m4 = base.clone();
    m4.push((
        999,
        r#"Source: "x"; DestDir: "{autopf}\Git\bin""#.to_string(),
    ));
    let err = check_no_foreign_locations(&m4).unwrap_err();
    assert!(
        err.contains("absolute path") && err.contains("Program Files"),
        "folded {{autopf}} must trip the drive-letter and 'Program Files' scans: {err}"
    );

    // 5. Commented-out `;PrivilegesRequired=lowest` (with or without admin override)
    let m5: Vec<(usize, String)> = base
        .iter()
        .map(|(n, l)| {
            if l.contains("PrivilegesRequired=lowest") {
                (*n, "; PrivilegesRequired=lowest".to_string())
            } else {
                (*n, l.clone())
            }
        })
        .collect();
    let err = check_lowest_privileges(&m5).unwrap_err();
    assert!(
        err.contains("non-comment PrivilegesRequired"),
        "rejection must name the privileges directive: {err}"
    );

    // 6. `{sys}\cmd.exe /c rd /s /q {autopf}\Git` in [Run]
    let mut m6 = base.clone();
    let run_idx = m6
        .iter()
        .position(|(_, l)| l.trim() == "[Run]")
        .expect("[Run] section exists");
    m6.insert(
        run_idx + 1,
        (
            999,
            r#"Filename: "{sys}\cmd.exe"; Parameters: "/c rd /s /q {autopf}\Git""#.to_string(),
        ),
    );
    let err = check_run_and_sys_binaries(&m6).unwrap_err();
    assert!(
        err.contains("{sys}\\taskkill.exe"),
        "rejection must name the sanctioned {{sys}} binary: {err}"
    );
    assert!(
        check_no_foreign_locations(&m6)
            .unwrap_err()
            .contains("Program Files"),
        "folded {{autopf}} in Parameters must surface as a foreign location"
    );

    // 7. Unsanctioned `{sys}\powershell.exe` in [Code]
    let mut m7 = base.clone();
    m7.push((999, r#"  Exec(ExpandConstant('{sys}\powershell.exe'), '', '', SW_HIDE, ewWaitUntilTerminated, ResultCode);"#.to_string()));
    let err = check_run_and_sys_binaries(&m7).unwrap_err();
    assert!(
        err.contains("{sys}") && err.contains("taskkill"),
        "rejection must name the unsanctioned {{sys}} reference: {err}"
    );

    // 8. `{syswow64}\cmd.exe` — the {sys} allow-list only sees the literal
    // `{sys}` spelling; the system-directory alias must fold into the
    // foreign-location scan instead.
    let mut m8 = base.clone();
    m8.push((999, r#"  Exec(ExpandConstant('{syswow64}\cmd.exe'), '/c rd /s /q {app}\x', '', SW_HIDE, ewWaitUntilTerminated, ResultCode);"#.to_string()));
    let err = check_no_foreign_locations(&m8).unwrap_err();
    assert!(
        err.contains("absolute path"),
        "folded {{syswow64}} must surface as a foreign absolute path: {err}"
    );

    // 9. `{sys}\taskkill.exe2` — a suffix continuing past the sanctioned
    // binary name is a different file, not taskkill.exe.
    let mut m9 = base.clone();
    m9.push((999, r#"  Exec(ExpandConstant('{sys}\taskkill.exe2'), '/IM niu.exe /F', '', SW_HIDE, ewWaitUntilTerminated, ResultCode);"#.to_string()));
    let err = check_run_and_sys_binaries(&m9).unwrap_err();
    assert!(
        err.contains("{sys}"),
        "rejection must name the {{sys}} reference: {err}"
    );

    // 10. `{commonappdata}` — a foreign root outside the Program Files
    // family still folds to an absolute location.
    let mut m10 = base;
    m10.push((
        999,
        r#"  SaveStringToFile(ExpandConstant('{commonappdata}\Git\marker'), 'x', False);"#
            .to_string(),
    ));
    let err = check_no_foreign_locations(&m10).unwrap_err();
    assert!(
        err.contains("absolute path"),
        "folded {{commonappdata}} must surface as a foreign absolute path: {err}"
    );
}
