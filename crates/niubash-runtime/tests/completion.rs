//! End-to-end smoke for the completion pipeline.
//!
//! Builds a CompletionState, registers a fixture dir, then asks for
//! `rg -<Tab>` completions and asserts the expected flags are returned.

use niubash_runtime::completion::external::{CommandDef, FlagDef};
use niubash_runtime::completion::runtime::{RuntimeCompletionCommand, RuntimeCompletionPlugin};
use niubash_runtime::completion::{
    CompletionBehavior, CompletionContext, CompletionMatchMode, CompletionState,
};
use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[test]
fn loads_toml_definitions_from_dir() {
    let fixture_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("tests")
        .join("fixtures")
        .join("completions");

    let state = Arc::new(Mutex::new(CompletionState::new(PathBuf::from("."))));
    {
        let mut s = state.lock().unwrap();
        s.load_completion_dirs(&[fixture_dir]);
    }

    // Build a context where the cursor is right after `rg -`
    let input = "rg -".to_string();
    let ctx = CompletionContext::new(PathBuf::from("."), input.clone(), input.len());

    let s = state.lock().unwrap();
    let suggestions: Vec<String> = s
        .plugins
        .iter()
        .flat_map(|p| p.complete(&ctx).map(|r| r.completions).unwrap_or_default())
        .collect();

    // We expect at least the long flags we defined in rg.toml
    assert!(
        suggestions.iter().any(|s| s == "--ignore-case"),
        "expected --ignore-case in suggestions, got: {:?}",
        suggestions
    );
    assert!(
        suggestions.iter().any(|s| s == "--regexp"),
        "expected --regexp in suggestions, got: {:?}",
        suggestions
    );
    assert!(
        suggestions.iter().any(|s| s == "--type"),
        "expected --type in suggestions, got: {:?}",
        suggestions
    );
}

#[test]
fn runtime_does_not_load_winuxcmd_definitions_without_bundle_or_user_dirs() {
    let state = Arc::new(Mutex::new(CompletionState::new(PathBuf::from("."))));
    {
        let mut s = state.lock().unwrap();
        s.load_completion_dirs(&[]);
    }

    assert_not_suggests(&state, "ls -", "--all");
    assert_not_suggests(&state, "grep -", "--ignore-case");
    assert_not_suggests(&state, "find -", "-name");
}

#[test]
fn command_completion_handles_empty_and_partial_command_words() {
    let state = Arc::new(Mutex::new(CompletionState::new(PathBuf::from("."))));
    {
        let mut s = state.lock().unwrap();
        s.load_completion_dirs(&[]);
    }

    assert_suggests(&state, "", "ls");
    assert_suggests(&state, "gre", "grep");
}

#[test]
fn substring_behavior_applies_to_loaded_flag_definitions() {
    let temp_dir = unique_temp_dir("niubash-substring-loaded-completion");
    std::fs::create_dir_all(&temp_dir).unwrap();
    std::fs::write(
        temp_dir.join("grep.toml"),
        r#"
command = "grep"
description = "test grep"

[[flags]]
long = "--ignore-case"
description = "fixture flag"
"#,
    )
    .unwrap();

    let state = Arc::new(Mutex::new(CompletionState::new(PathBuf::from("."))));
    {
        let mut s = state.lock().unwrap();
        s.load_completion_dirs(&[temp_dir.clone()]);
    }

    assert_not_suggests(&state, "grep -case", "--ignore-case");
    assert_suggests_with_behavior(
        &state,
        "grep -case",
        "--ignore-case",
        CompletionBehavior {
            match_mode: CompletionMatchMode::Substring,
            ..CompletionBehavior::default()
        },
    );

    let _ = std::fs::remove_dir_all(temp_dir);
}

#[test]
fn user_toml_loads_command_definition_without_runtime_defaults() {
    let temp_dir = unique_temp_dir("niubash-completion-override");
    std::fs::create_dir_all(&temp_dir).unwrap();
    std::fs::write(
        temp_dir.join("ls.toml"),
        r#"
command = "ls"
description = "test override"

[[flags]]
long = "--custom-only"
description = "fixture override flag"
"#,
    )
    .unwrap();

    let state = Arc::new(Mutex::new(CompletionState::new(PathBuf::from("."))));
    {
        let mut s = state.lock().unwrap();
        s.load_completion_dirs(&[temp_dir.clone()]);
    }

    assert_suggests(&state, "ls -", "--custom-only");
    assert_not_suggests(&state, "ls -", "--all");

    let _ = std::fs::remove_dir_all(temp_dir);
}

#[test]
fn injected_definitions_are_loaded_before_user_dirs() {
    let imported = CommandDef {
        command: "fixture-tool".to_string(),
        description: Some("injected completion definition".to_string()),
        flags: vec![FlagDef {
            short: None,
            long: Some("--injected".to_string()),
            description: Some("injected fixture flag".to_string()),
            takes_value: false,
            values_source: None,
        }],
        subcommands: Vec::new(),
    };

    let state = Arc::new(Mutex::new(CompletionState::new(PathBuf::from("."))));
    {
        let mut s = state.lock().unwrap();
        s.load_completion_dirs_with_definitions(&[], vec![imported]);
    }

    assert_suggests(&state, "fixture-tool -", "--injected");
}

#[test]
fn injected_definitions_load_without_runtime_defaults() {
    let imported = CommandDef {
        command: "ls".to_string(),
        description: Some("injected ls completion".to_string()),
        flags: vec![FlagDef {
            short: None,
            long: Some("--injected-extra".to_string()),
            description: Some("extra injected flag".to_string()),
            takes_value: false,
            values_source: None,
        }],
        subcommands: Vec::new(),
    };

    let state = Arc::new(Mutex::new(CompletionState::new(PathBuf::from("."))));
    {
        let mut s = state.lock().unwrap();
        s.load_completion_dirs_with_definitions(&[], vec![imported]);
    }

    assert_not_suggests(&state, "ls -", "--all");
    assert_suggests(&state, "ls -", "--injected-extra");
}

#[test]
fn user_toml_overrides_injected_definitions() {
    let temp_dir = unique_temp_dir("niubash-injected-completion-override");
    std::fs::create_dir_all(&temp_dir).unwrap();
    std::fs::write(
        temp_dir.join("fixture-tool.toml"),
        r#"
command = "fixture-tool"
description = "user override"

[[flags]]
long = "--user-only"
description = "user override flag"
"#,
    )
    .unwrap();

    let imported = CommandDef {
        command: "fixture-tool".to_string(),
        description: Some("injected completion definition".to_string()),
        flags: vec![FlagDef {
            short: None,
            long: Some("--injected".to_string()),
            description: Some("injected fixture flag".to_string()),
            takes_value: false,
            values_source: None,
        }],
        subcommands: Vec::new(),
    };

    let state = Arc::new(Mutex::new(CompletionState::new(PathBuf::from("."))));
    {
        let mut s = state.lock().unwrap();
        s.load_completion_dirs_with_definitions(&[temp_dir.clone()], vec![imported]);
    }

    assert_suggests(&state, "fixture-tool -", "--user-only");
    assert_not_suggests(&state, "fixture-tool -", "--injected");

    let _ = std::fs::remove_dir_all(temp_dir);
}

#[test]
fn runtime_completion_provider_runs_allowed_command_with_current_words() {
    let _lock = env_lock().lock().unwrap();
    let _env = EnvGuard::capture(&["PATH"]);
    let temp_dir = unique_temp_dir("niubash-runtime-completion-provider");
    std::fs::create_dir_all(&temp_dir).unwrap();
    std::fs::write(
        temp_dir.join("npm.cmd"),
        r#"@echo off
if "%1"=="completion" if "%2"=="--" goto complete
exit /b 2
:complete
echo build
echo bundle
echo test
"#,
    )
    .unwrap();

    let old_path = std::env::var_os("PATH").unwrap_or_default();
    let mut path_entries = vec![temp_dir.clone()];
    path_entries.extend(std::env::split_paths(&old_path));
    std::env::set_var("PATH", std::env::join_paths(path_entries).unwrap());

    let state = Arc::new(Mutex::new(CompletionState::new(PathBuf::from("."))));
    {
        let mut s = state.lock().unwrap();
        s.add_plugin(Arc::new(RuntimeCompletionPlugin::new(
            vec![RuntimeCompletionCommand {
                command: "npm".to_string(),
                args: vec!["completion".to_string(), "--".to_string()],
                origin: "test".to_string(),
            }],
            Duration::from_secs(2),
        )));
    }

    assert_suggests(&state, "npm run b", "build");
    assert_suggests(&state, "npm run b", "bundle");
    assert_not_suggests(&state, "npm run b", "test");
    assert_suggests_with_behavior(
        &state,
        "npm run und",
        "bundle",
        CompletionBehavior {
            match_mode: CompletionMatchMode::Substring,
            ..CompletionBehavior::default()
        },
    );

    let _ = std::fs::remove_dir_all(temp_dir);
}

fn suggestions_for(state: &Arc<Mutex<CompletionState>>, input: &str) -> Vec<String> {
    suggestions_for_behavior(state, input, CompletionBehavior::default())
}

fn suggestions_for_behavior(
    state: &Arc<Mutex<CompletionState>>,
    input: &str,
    behavior: CompletionBehavior,
) -> Vec<String> {
    let ctx = CompletionContext::with_behavior(
        PathBuf::from("."),
        input.to_string(),
        input.len(),
        behavior,
    );
    let s = state.lock().unwrap();
    s.plugins
        .iter()
        .flat_map(|p| p.complete(&ctx).map(|r| r.completions).unwrap_or_default())
        .collect()
}

fn assert_suggests(state: &Arc<Mutex<CompletionState>>, input: &str, expected: &str) {
    let suggestions = suggestions_for(state, input);
    assert!(
        suggestions.iter().any(|s| s == expected),
        "expected {expected} for {input:?}, got: {:?}",
        suggestions
    );
}

fn assert_not_suggests(state: &Arc<Mutex<CompletionState>>, input: &str, unexpected: &str) {
    let suggestions = suggestions_for(state, input);
    assert!(
        !suggestions.iter().any(|s| s == unexpected),
        "did not expect {unexpected} for {input:?}, got: {:?}",
        suggestions
    );
}

fn assert_suggests_with_behavior(
    state: &Arc<Mutex<CompletionState>>,
    input: &str,
    expected: &str,
    behavior: CompletionBehavior,
) {
    let suggestions = suggestions_for_behavior(state, input, behavior);
    assert!(
        suggestions.iter().any(|s| s == expected),
        "expected {expected} for {input:?}, got: {:?}",
        suggestions
    );
}

fn unique_temp_dir(prefix: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!("{}-{}-{}", prefix, std::process::id(), nanos))
}

fn env_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

struct EnvGuard {
    saved: Vec<(String, Option<OsString>)>,
}

impl EnvGuard {
    fn capture(names: &[&str]) -> Self {
        Self {
            saved: names
                .iter()
                .map(|name| ((*name).to_string(), std::env::var_os(name)))
                .collect(),
        }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        for (name, value) in &self.saved {
            match value {
                Some(value) => std::env::set_var(name, value),
                None => std::env::remove_var(name),
            }
        }
    }
}
