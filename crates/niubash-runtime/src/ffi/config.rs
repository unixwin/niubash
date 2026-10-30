//! The `NIU_WINUXCMD_FFI*` switch family (DESIGN §3.1, names as
//! ruled: the `NIU_WINUXCMD_FFI` prefix, not `NIU_FFI_*`).
//!
//! | Variable | Meaning | Default |
//! |---|---|---|
//! | `NIU_WINUXCMD_FFI` | master switch: `1` on, `0`/unset off | off |
//! | `NIU_WINUXCMD_FFI_EXCLUDE` | comma-separated per-command exclusions | empty |
//! | `NIU_WINUXCMD_FFI_DLL` | DLL absolute-path override (relative is ignored with a warning) | empty |
//! | `NIU_WINUXCMD_FFI_LOG` | `1` logs each route decision to stderr | off |
//!
//! The master switch's default lives in [`DEFAULT_ENABLED`] and
//! nowhere else: the #567 ruling flips FFI to default-on only when
//! the zero-problem gate passes in full (round-2 differential,
//! end-to-end four-arm equivalence, long-task Ctrl-C proof,
//! invisible fallback, route census). Gate items ① and ③ are open
//! at this delivery, so the constant is `false`. An explicit
//! `NIU_WINUXCMD_FFI=0` is the permanent kill switch: it means off
//! regardless of the compiled default, and the status face reports
//! it as its own state so a user-forced off is never confused with
//! an unavailable DLL.

use std::collections::BTreeSet;
use std::path::PathBuf;

/// The compiled-in default for the master switch (#567 gate: flip
/// this single constant — and only this — when every gate item has
/// passed; an explicit env `0` still overrides it off).
pub const DEFAULT_ENABLED: bool = false;

/// How the master switch was set, for honest status reporting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SwitchState {
    /// Not set in the environment; [`DEFAULT_ENABLED`] decides.
    Default,
    /// Explicitly set to a non-`1` value: the kill switch.
    ExplicitOff,
    /// Explicitly set to `1`.
    ExplicitOn,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FfiConfig {
    pub switch: SwitchState,
    /// Effective on/off after the switch and the compiled default.
    pub enabled: bool,
    /// Per-command exclusions (user control, DESIGN §2.2). Matched
    /// ASCII-case-insensitively, like Windows command resolution.
    pub exclude: BTreeSet<String>,
    /// DLL absolute-path override (development/diagnostics).
    pub dll_override: Option<PathBuf>,
    /// The override was set but relative: discovery ignores it and
    /// the status face carries this warning (§2.3: a user-supplied
    /// bad path is worth one line, unlike a silent absence).
    pub dll_override_relative: bool,
    /// Per-route-decision stderr logging (`NIU_WINUXCMD_FFI_LOG=1`).
    pub log_routes: bool,
}

impl FfiConfig {
    pub fn from_env() -> Self {
        Self::from_pairs(std::env::vars())
    }

    /// Injectable source so tests never touch the process env.
    pub fn from_pairs<I, K, V>(pairs: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: AsRef<str>,
        V: AsRef<str>,
    {
        let collected: Vec<(String, String)> = pairs
            .into_iter()
            .map(|(k, v)| (k.as_ref().to_string(), v.as_ref().to_string()))
            .collect();
        Self::from_lookup(|name| {
            collected
                .iter()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v.clone())
        })
    }

    fn from_lookup<F: FnMut(&str) -> Option<String>>(mut get: F) -> Self {
        let switch = match get("NIU_WINUXCMD_FFI") {
            None => SwitchState::Default,
            Some(v) if v == "1" => SwitchState::ExplicitOn,
            Some(_) => SwitchState::ExplicitOff,
        };
        let enabled = match switch {
            SwitchState::ExplicitOn => true,
            SwitchState::ExplicitOff => false,
            SwitchState::Default => DEFAULT_ENABLED,
        };
        let exclude = get("NIU_WINUXCMD_FFI_EXCLUDE")
            .map(|raw| {
                raw.split(',')
                    .map(str::trim)
                    .filter(|name| !name.is_empty())
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        let (dll_override, dll_override_relative) = match get("NIU_WINUXCMD_FFI_DLL") {
            Some(raw) if !raw.trim().is_empty() => {
                let path = PathBuf::from(raw.trim());
                if path.is_absolute() {
                    (Some(path), false)
                } else {
                    (None, true)
                }
            }
            _ => (None, false),
        };
        let log_routes = matches!(get("NIU_WINUXCMD_FFI_LOG").as_deref(), Some("1"));
        FfiConfig {
            switch,
            enabled,
            exclude,
            dll_override,
            dll_override_relative,
            log_routes,
        }
    }

    /// Whether `name` is on the user's exclusion list.
    pub fn is_excluded(&self, name: &str) -> bool {
        self.exclude
            .iter()
            .any(|entry| entry.eq_ignore_ascii_case(name))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(pairs: &[(&str, &str)]) -> FfiConfig {
        FfiConfig::from_pairs(pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())))
    }

    #[test]
    fn unset_switch_uses_compiled_default() {
        let cfg = config(&[]);
        assert_eq!(cfg.switch, SwitchState::Default);
        assert_eq!(cfg.enabled, DEFAULT_ENABLED);
        assert!(!DEFAULT_ENABLED, "gate ①③ open: default must stay off");
    }

    #[test]
    fn explicit_on_and_kill_switch() {
        let on = config(&[("NIU_WINUXCMD_FFI", "1")]);
        assert_eq!(on.switch, SwitchState::ExplicitOn);
        assert!(on.enabled);
        let off = config(&[("NIU_WINUXCMD_FFI", "0")]);
        assert_eq!(off.switch, SwitchState::ExplicitOff);
        assert!(!off.enabled);
        let other = config(&[("NIU_WINUXCMD_FFI", "yes")]);
        assert_eq!(other.switch, SwitchState::ExplicitOff);
        assert!(!other.enabled);
    }

    #[test]
    fn exclude_list_parses_and_matches_case_insensitively() {
        let cfg = config(&[("NIU_WINUXCMD_FFI_EXCLUDE", " grep, sort ,,")]);
        assert!(cfg.is_excluded("grep"));
        assert!(cfg.is_excluded("GREP"));
        assert!(cfg.is_excluded("sort"));
        assert!(!cfg.is_excluded("wc"));
    }

    #[test]
    fn relative_dll_override_is_ignored_with_warning_flag() {
        let rel = config(&[("NIU_WINUXCMD_FFI_DLL", "ffi/winuxcore.dll")]);
        assert_eq!(rel.dll_override, None);
        assert!(rel.dll_override_relative);
        let abs_path = if cfg!(windows) {
            "C:\\winuxcmd\\usr\\bin\\ffi\\v1\\winuxcore.dll"
        } else {
            "/winuxcmd/usr/bin/ffi/v1/winuxcore.dll"
        };
        let abs = config(&[("NIU_WINUXCMD_FFI_DLL", abs_path)]);
        assert!(abs.dll_override.is_some());
        assert!(!abs.dll_override_relative);
    }

    #[test]
    fn log_flag() {
        assert!(config(&[("NIU_WINUXCMD_FFI_LOG", "1")]).log_routes);
        assert!(!config(&[]).log_routes);
    }
}
