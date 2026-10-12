use anyhow::{Context, Result};
use serde_json::{json, Map, Value};
use std::fs;
use std::path::{Path, PathBuf};

pub const NIU_PROFILE_GUID: &str = "{7d8b7341-8f4d-4c56-91f0-4ad220d41db1}";
/// The WT profile name the upsert registers. Public because the shortcut
/// form (niubash#211) addresses the profile by this name in
/// `wt.exe -p <name>` arguments.
pub const NIU_PROFILE_NAME: &str = "Niubash";
const PROFILE_NAME: &str = NIU_PROFILE_NAME;

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ProfileInstallSummary {
    pub updated: Vec<PathBuf>,
    pub skipped: Vec<PathBuf>,
}

pub fn install_niubash_profile(
    commandline: &Path,
    icon: Option<&Path>,
    set_default: bool,
    font_face: Option<&str>,
) -> Result<ProfileInstallSummary> {
    install_niubash_profile_in_settings(
        commandline,
        icon,
        set_default,
        font_face,
        candidate_settings_paths(),
    )
}

/// Set `font.face` on an existing Niubash profile without creating one.
/// Settings files without the profile are left untouched.
pub fn set_niubash_profile_font(face: &str) -> Result<ProfileInstallSummary> {
    let mut summary = ProfileInstallSummary::default();
    for settings_path in candidate_settings_paths() {
        if !settings_path.is_file() {
            summary.skipped.push(settings_path);
            continue;
        }
        let mut root = read_settings_or_default(&settings_path)?;
        if !niubash_profile_exists(&root) {
            summary.skipped.push(settings_path);
            continue;
        }
        set_profile_font(&mut root, face);
        let formatted = serde_json::to_string_pretty(&root)?;
        fs::write(&settings_path, format!("{formatted}\n"))
            .with_context(|| format!("write {}", settings_path.display()))?;
        summary.updated.push(settings_path);
    }
    Ok(summary)
}

pub fn install_niubash_profile_in_settings<I>(
    commandline: &Path,
    icon: Option<&Path>,
    set_default: bool,
    font_face: Option<&str>,
    settings_paths: I,
) -> Result<ProfileInstallSummary>
where
    I: IntoIterator<Item = PathBuf>,
{
    let mut summary = ProfileInstallSummary::default();

    for settings_path in settings_paths {
        let should_write =
            settings_path.is_file() || settings_path.parent().is_some_and(|parent| parent.is_dir());
        if !should_write {
            summary.skipped.push(settings_path);
            continue;
        }

        let mut root = read_settings_or_default(&settings_path)?;
        upsert_profile(&mut root, commandline, icon, set_default, font_face);
        let formatted = serde_json::to_string_pretty(&root)?;
        if let Some(parent) = settings_path.parent() {
            fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
        }
        fs::write(&settings_path, format!("{formatted}\n"))
            .with_context(|| format!("write {}", settings_path.display()))?;
        summary.updated.push(settings_path);
    }

    Ok(summary)
}

/// True when a Niubash profile exists in at least one candidate WT
/// settings.json on this machine. The shortcut form (niubash#211) gates its
/// `wt.exe -p` branch on this: `wt -p` against an unregistered profile name
/// errors out, so a machine whose profile install was skipped (or whose
/// settings directories never appeared) must keep the conhost baseline.
pub fn niubash_profile_registered() -> bool {
    niubash_profile_registered_in(&candidate_settings_paths())
}

/// Testable form of [`niubash_profile_registered`]: checks explicit
/// settings paths, so tests can stage stable + Preview layouts.
pub fn niubash_profile_registered_in(settings_paths: &[PathBuf]) -> bool {
    settings_paths
        .iter()
        .filter(|path| path.is_file())
        .any(|path| {
            read_settings_or_default(path)
                .map(|root| niubash_profile_exists(&root))
                .unwrap_or(false)
        })
}

fn candidate_settings_paths() -> Vec<PathBuf> {
    let Some(local_app_data) = dirs::data_local_dir() else {
        return Vec::new();
    };

    vec![
        local_app_data
            .join("Packages")
            .join("Microsoft.WindowsTerminal_8wekyb3d8bbwe")
            .join("LocalState")
            .join("settings.json"),
        local_app_data
            .join("Packages")
            .join("Microsoft.WindowsTerminalPreview_8wekyb3d8bbwe")
            .join("LocalState")
            .join("settings.json"),
        local_app_data
            .join("Microsoft")
            .join("Windows Terminal")
            .join("settings.json"),
    ]
}

fn read_settings_or_default(settings_path: &Path) -> Result<Value> {
    if !settings_path.is_file() {
        return Ok(default_settings());
    }

    let raw = fs::read_to_string(settings_path)
        .with_context(|| format!("read {}", settings_path.display()))?;
    if raw.trim().is_empty() {
        return Ok(default_settings());
    }

    let parsed: Value =
        serde_json::from_str(&raw).with_context(|| format!("parse {}", settings_path.display()))?;
    if parsed.is_object() {
        Ok(parsed)
    } else {
        Ok(default_settings())
    }
}

fn default_settings() -> Value {
    json!({
        "$schema": "https://aka.ms/terminal-profiles-schema",
        "profiles": {
            "list": []
        }
    })
}

fn upsert_profile(
    root: &mut Value,
    commandline: &Path,
    icon: Option<&Path>,
    set_default: bool,
    font_face: Option<&str>,
) {
    let profile = niubash_profile(commandline, icon, font_face);

    {
        let list = profiles_list_mut(root);
        if let Some(existing) = list.iter_mut().find(|candidate| {
            candidate
                .get("guid")
                .and_then(Value::as_str)
                .is_some_and(|guid| guid.eq_ignore_ascii_case(NIU_PROFILE_GUID))
                || candidate
                    .get("name")
                    .and_then(Value::as_str)
                    .is_some_and(|name| name.eq_ignore_ascii_case(PROFILE_NAME))
        }) {
            merge_profile(existing, profile);
        } else {
            list.push(profile);
        }
    }

    if set_default || has_invalid_default_profile(root) {
        root.as_object_mut()
            .expect("settings root is an object")
            .insert("defaultProfile".to_string(), json!(NIU_PROFILE_GUID));
    }
}

fn niubash_profile(commandline: &Path, icon: Option<&Path>, font_face: Option<&str>) -> Value {
    let mut profile = json!({
        "guid": NIU_PROFILE_GUID,
        "name": PROFILE_NAME,
        "commandline": commandline.to_string_lossy(),
        "startingDirectory": "%USERPROFILE%",
        "hidden": false
    });

    if let Some(icon) = icon {
        profile
            .as_object_mut()
            .expect("profile is an object")
            .insert("icon".to_string(), json!(icon.to_string_lossy()));
    }
    if let Some(face) = font_face {
        profile
            .as_object_mut()
            .expect("profile is an object")
            .insert("font".to_string(), json!({ "face": face }));
    }

    profile
}

fn niubash_profile_exists(root: &Value) -> bool {
    profiles_list(root).into_iter().flatten().any(|profile| {
        profile
            .get("guid")
            .and_then(Value::as_str)
            .is_some_and(|guid| guid.eq_ignore_ascii_case(NIU_PROFILE_GUID))
            || profile
                .get("name")
                .and_then(Value::as_str)
                .is_some_and(|name| name.eq_ignore_ascii_case(PROFILE_NAME))
    })
}

fn set_profile_font(root: &mut Value, face: &str) {
    let list = profiles_list_mut(root);
    if let Some(existing) = list.iter_mut().find(|candidate| {
        candidate
            .get("guid")
            .and_then(Value::as_str)
            .is_some_and(|guid| guid.eq_ignore_ascii_case(NIU_PROFILE_GUID))
            || candidate
                .get("name")
                .and_then(Value::as_str)
                .is_some_and(|name| name.eq_ignore_ascii_case(PROFILE_NAME))
    }) {
        let profile = ensure_object(existing);
        profile.insert("font".to_string(), json!({ "face": face }));
    }
}

fn merge_profile(existing: &mut Value, updates: Value) {
    let existing = ensure_object(existing);
    let updates = updates
        .as_object()
        .expect("profile updates should be an object");
    for (key, value) in updates {
        existing.insert(key.clone(), value.clone());
    }
}

fn profiles_list_mut(root: &mut Value) -> &mut Vec<Value> {
    let root_object = ensure_object(root);
    let profiles = root_object
        .entry("profiles".to_string())
        .or_insert_with(|| json!({ "list": [] }));

    if profiles.is_array() {
        return profiles.as_array_mut().expect("profiles is array");
    }

    let profiles_object = ensure_object(profiles);
    profiles_object
        .entry("list".to_string())
        .or_insert_with(|| json!([]));
    profiles_object
        .get_mut("list")
        .expect("profiles list exists")
        .as_array_mut()
        .expect("profiles list is array")
}

fn has_invalid_default_profile(root: &Value) -> bool {
    let Some(default_profile) = root.get("defaultProfile").and_then(Value::as_str) else {
        return false;
    };
    let default_profile = default_profile.trim();
    !default_profile.is_empty() && !profile_guid_exists(root, default_profile)
}

fn profile_guid_exists(root: &Value, guid: &str) -> bool {
    profiles_list(root).into_iter().flatten().any(|profile| {
        profile
            .get("guid")
            .and_then(Value::as_str)
            .is_some_and(|candidate| candidate.eq_ignore_ascii_case(guid))
    })
}

fn profiles_list(root: &Value) -> Option<&Vec<Value>> {
    let profiles = root.get("profiles")?;
    if let Some(list) = profiles.as_array() {
        return Some(list);
    }
    profiles.get("list")?.as_array()
}

fn ensure_object(value: &mut Value) -> &mut Map<String, Value> {
    if !value.is_object() {
        *value = Value::Object(Map::new());
    }
    value.as_object_mut().expect("value is object")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_settings_with_niubash_profile() {
        let temp = tempfile::tempdir().unwrap();
        let settings = temp.path().join("settings.json");

        let summary = install_niubash_profile_in_settings(
            Path::new("C:/Users/me/AppData/Local/Programs/Niubash/niu.exe"),
            Some(Path::new(
                "C:/Users/me/AppData/Local/Programs/Niubash/assets/niubash-icon-256.png",
            )),
            false,
            None,
            [settings.clone()],
        )
        .unwrap();

        assert_eq!(summary.updated, vec![settings.clone()]);
        let root: Value = serde_json::from_str(&fs::read_to_string(settings).unwrap()).unwrap();
        let profile = &root["profiles"]["list"][0];
        assert_eq!(profile["name"], "Niubash");
        assert_eq!(profile["guid"], NIU_PROFILE_GUID);
        assert_eq!(profile["startingDirectory"], "%USERPROFILE%");
        assert!(root.get("defaultProfile").is_none());
    }

    #[test]
    fn updates_existing_profile_and_can_set_default() {
        let temp = tempfile::tempdir().unwrap();
        let settings = temp.path().join("settings.json");
        fs::write(
            &settings,
            serde_json::to_string_pretty(&json!({
                "profiles": {
                    "list": [
                        {
                            "guid": NIU_PROFILE_GUID,
                            "name": "Old Niubash",
                            "colorScheme": "Campbell"
                        }
                    ]
                }
            }))
            .unwrap(),
        )
        .unwrap();

        install_niubash_profile_in_settings(
            Path::new("D:/Apps/Niubash/niu.exe"),
            None,
            true,
            None,
            [settings.clone()],
        )
        .unwrap();

        let root: Value = serde_json::from_str(&fs::read_to_string(settings).unwrap()).unwrap();
        let list = root["profiles"]["list"].as_array().unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0]["name"], "Niubash");
        assert_eq!(list[0]["colorScheme"], "Campbell");
        assert_eq!(root["defaultProfile"], NIU_PROFILE_GUID);
    }

    #[test]
    fn repairs_invalid_default_profile_when_upserting() {
        let temp = tempfile::tempdir().unwrap();
        let settings = temp.path().join("settings.json");
        fs::write(
            &settings,
            serde_json::to_string_pretty(&json!({
                "defaultProfile": "{missing-profile}",
                "profiles": {
                    "list": [
                        {
                            "guid": "{61c54bbd-c2c6-5271-96e7-009a87ff44bf}",
                            "name": "Windows PowerShell"
                        }
                    ]
                }
            }))
            .unwrap(),
        )
        .unwrap();

        install_niubash_profile_in_settings(
            Path::new("D:/Apps/Niubash/niu.exe"),
            None,
            false,
            None,
            [settings.clone()],
        )
        .unwrap();

        let root: Value = serde_json::from_str(&fs::read_to_string(settings).unwrap()).unwrap();
        assert_eq!(root["defaultProfile"], NIU_PROFILE_GUID);
    }

    #[test]
    fn profile_registration_detected_in_any_candidate() {
        let temp = tempfile::tempdir().unwrap();
        let stable = temp.path().join("stable").join("settings.json");
        let preview = temp.path().join("preview").join("settings.json");
        fs::create_dir_all(stable.parent().unwrap()).unwrap();
        fs::create_dir_all(preview.parent().unwrap()).unwrap();
        fs::write(&stable, json!({ "profiles": { "list": [] } }).to_string()).unwrap();
        fs::write(
            &preview,
            serde_json::to_string_pretty(&json!({
                "profiles": {
                    "list": [ { "guid": NIU_PROFILE_GUID, "name": PROFILE_NAME } ]
                }
            }))
            .unwrap(),
        )
        .unwrap();

        // Multi-channel machine (stable + Preview both installed): the
        // profile registered under Preview alone counts as registered.
        assert!(niubash_profile_registered_in(&[
            stable.clone(),
            preview.clone()
        ]));

        // And the same check over stable alone stays false — the probe is
        // per settings file, not per machine-wide assumption.
        assert!(!niubash_profile_registered_in(&[stable]));
    }

    #[test]
    fn profile_registration_false_for_missing_or_profileless_settings() {
        let temp = tempfile::tempdir().unwrap();
        let missing = temp.path().join("missing").join("settings.json");
        let profileless = temp.path().join("profileless").join("settings.json");
        fs::create_dir_all(profileless.parent().unwrap()).unwrap();
        fs::write(
            &profileless,
            json!({ "profiles": { "list": [] } }).to_string(),
        )
        .unwrap();

        assert!(!niubash_profile_registered_in(&[missing]));
        assert!(!niubash_profile_registered_in(&[profileless]));
    }
}
