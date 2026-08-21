//! Binary-level tests for the Winuxsh-native plugin inventory commands.
use sha2::{Digest, Sha256};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};
fn winuxsh_binary() -> PathBuf {
    let p = PathBuf::from(env!("CARGO_BIN_EXE_winuxsh"));
    if p.exists() {
        return p;
    }
    let mut fallback = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    fallback.push("target");
    fallback.push("debug");
    fallback.push(if cfg!(windows) {
        "winuxsh.exe"
    } else {
        "winuxsh"
    });
    fallback
}
#[test]
fn plugin_list_text_lists_official_packs() {
    let output = run_winuxsh(&["plugin", "list"]);
    assert_success(&output, "plugin list text");
    let stdout = stdout_text(&output);
    assert!(stdout.contains("Official Winuxsh plugins"), "{stdout}");
    assert!(stdout.contains("Bundle: oh-my-winuxsh"), "{stdout}");
    assert!(stdout.contains("Source: compiled_fallback"), "{stdout}");
    assert!(stdout.contains("Trust source: official_bundle"), "{stdout}");
    assert!(
        stdout.contains("These are Winuxsh-native packs"),
        "{stdout}"
    );
    assert!(
        stdout.contains("- git kind=builtin category=devtools default=on"),
        "{stdout}"
    );
    assert!(
        stdout.contains("- zoxide kind=builtin category=workflow default=off"),
        "{stdout}"
    );
    assert!(
        stdout.contains("- keybindings kind=builtin category=ux default=on"),
        "{stdout}"
    );
    assert!(
        stdout.contains(
            "- themes kind=builtin category=ux default=on execution=none_declarative externalization=declarative_asset"
        ),
        "{stdout}"
    );
    assert!(
        stdout.contains(
            "- command-not-found kind=builtin category=hints default=off execution=host_builtin externalization=pure_provider_candidate"
        ),
        "{stdout}"
    );
}
#[test]
fn plugin_list_json_lists_machine_readable_packs() {
    let output = run_winuxsh(&["plugin", "list", "--json"]);
    assert_success(&output, "plugin list json");
    let stdout = stdout_text(&output);
    assert!(stdout.contains(r#""name": "git""#), "{stdout}");
    assert!(stdout.contains(r#""bundle": "oh-my-winuxsh""#), "{stdout}");
    assert!(
        stdout.contains(r#""source": "compiled_fallback""#),
        "{stdout}"
    );
    assert!(
        stdout.contains(r#""trust_source": "official_bundle""#),
        "{stdout}"
    );
    assert!(stdout.contains(r#""kind": "builtin""#), "{stdout}");
    assert!(stdout.contains(r#""category": "devtools""#), "{stdout}");
    assert!(
        stdout.contains(r#""execution_model": "none_declarative""#),
        "{stdout}"
    );
    assert!(
        stdout.contains(r#""externalization_class": "declarative_asset""#),
        "{stdout}"
    );
    assert!(
        stdout.contains(r#""externalization_class": "shell_effect_candidate""#),
        "{stdout}"
    );
    assert!(
        stdout.contains(r#""target_runtime": "asset_only_declarative_schema_tbd""#),
        "{stdout}"
    );
    assert!(stdout.contains(r#""shell_mutating": true"#), "{stdout}");
    assert!(stdout.contains(r#""fallback_needed": true"#), "{stdout}");
    assert!(stdout.contains(r#""name": "thefuck""#), "{stdout}");
}
#[test]
fn plugin_list_and_info_mark_external_bundle_review_only() {
    let temp = temp_dir("plugin-list-external-bundle");
    let bundle = temp.join("bundle");
    write_external_process_test_bundle(&bundle, "9.9.7");
    let text = run_winuxsh_with_env(
        &["plugin", "list"],
        &[("WINUXSH_PLUGIN_BUNDLE_PATH", bundle.clone())],
    );
    assert_success(&text, "plugin list external bundle text");
    let stdout = stdout_text(&text);
    assert!(stdout.contains("Winuxsh plugin inventory"), "{stdout}");
    assert!(stdout.contains("Bundle: community-tools"), "{stdout}");
    assert!(stdout.contains("Source: env_override"), "{stdout}");
    assert!(stdout.contains("Trust source: external_bundle"), "{stdout}");
    assert!(
        stdout.contains(
            "External bundle packs are review-only until registry trust policy is implemented."
        ),
        "{stdout}"
    );
    let json = run_winuxsh_with_env(
        &["plugin", "list", "--json"],
        &[("WINUXSH_PLUGIN_BUNDLE_PATH", bundle.clone())],
    );
    assert_success(&json, "plugin list external bundle json");
    let stdout = stdout_text(&json);
    assert!(
        stdout.contains(r#""bundle": "community-tools""#),
        "{stdout}"
    );
    assert!(stdout.contains(r#""source": "env_override""#), "{stdout}");
    assert!(
        stdout.contains(r#""trust_source": "external_bundle""#),
        "{stdout}"
    );
    let search = run_winuxsh_with_env(
        &["plugin", "search", "process-echo", "--json"],
        &[("WINUXSH_PLUGIN_BUNDLE_PATH", bundle.clone())],
    );
    assert_success(&search, "plugin search external bundle json");
    let stdout = stdout_text(&search);
    assert!(stdout.contains(r#""source": "env_override""#), "{stdout}");
    assert!(
        stdout.contains(r#""trust_source": "external_bundle""#),
        "{stdout}"
    );
    let info = run_winuxsh_with_env(
        &["plugin", "info", "process-echo"],
        &[("WINUXSH_PLUGIN_BUNDLE_PATH", bundle.clone())],
    );
    assert_success(&info, "plugin info external bundle text");
    let stdout = stdout_text(&info);
    assert!(stdout.contains("Plugin: process-echo"), "{stdout}");
    assert!(stdout.contains("Bundle: community-tools"), "{stdout}");
    assert!(stdout.contains("Source: env_override"), "{stdout}");
    assert!(stdout.contains("Trust source: external_bundle"), "{stdout}");
    let info_json = run_winuxsh_with_env(
        &["plugin", "info", "process-echo", "--json"],
        &[("WINUXSH_PLUGIN_BUNDLE_PATH", bundle.clone())],
    );
    assert_success(&info_json, "plugin info external bundle json");
    let stdout = stdout_text(&info_json);
    assert!(stdout.contains(r#""name": "process-echo""#), "{stdout}");
    assert!(stdout.contains(r#""source": "env_override""#), "{stdout}");
    assert!(
        stdout.contains(r#""trust_source": "external_bundle""#),
        "{stdout}"
    );
    let _ = fs::remove_dir_all(temp);
}
#[test]
fn plugin_search_text_discovers_matching_packs() {
    let output = run_winuxsh(&["plugin", "search", "devtools"]);
    assert_success(&output, "plugin search text");
    let stdout = stdout_text(&output);
    assert!(stdout.contains("Plugin search"), "{stdout}");
    assert!(stdout.contains("Source: compiled_fallback"), "{stdout}");
    assert!(stdout.contains("Trust source: official_bundle"), "{stdout}");
    assert!(stdout.contains("Query: devtools"), "{stdout}");
    assert!(
        stdout.contains("- git kind=builtin category=devtools default=on"),
        "{stdout}"
    );
    assert!(
        stdout.contains("- docker kind=builtin category=devtools default=off"),
        "{stdout}"
    );
    assert!(
        !stdout.contains("- prompts kind=builtin category=ux"),
        "{stdout}"
    );
}
#[test]
fn plugin_search_json_reports_matched_fields() {
    let output = run_winuxsh(&["plugin", "search", "zoxide", "--json"]);
    assert_success(&output, "plugin search json");
    let stdout = stdout_text(&output);
    assert!(stdout.contains("zoxide"), "{stdout}");
    assert!(stdout.contains("workflow"), "{stdout}");
    assert!(stdout.contains("matched_fields"), "{stdout}");
    assert!(
        stdout.contains(r#""source": "compiled_fallback""#),
        "{stdout}"
    );
    assert!(
        stdout.contains(r#""trust_source": "official_bundle""#),
        "{stdout}"
    );
}
#[test]
fn plugin_themes_lists_user_and_bundle_sources_only() {
    let temp = temp_dir("plugin-themes-catalog");
    let bundle = temp.join("bundle");
    write_theme_test_bundle(&bundle, "9.9.10");
    let envs = [("WINUXSH_PLUGIN_BUNDLE_PATH", bundle)];
    let text = run_winuxsh_with_env(&["plugin", "themes"], &envs);
    assert_success(&text, "plugin themes text");
    let stdout = stdout_text(&text);
    assert!(stdout.contains("Winuxsh themes"), "{stdout}");
    assert!(!stdout.contains("builtin_fallback"), "{stdout}");
    assert!(
        stdout.contains(
            "- testmarket source=bundle owner=oh-my-winuxsh@9.9.10 bundle=oh-my-winuxsh pack=themes"
        ),
        "{stdout}"
    );
    assert!(stdout.contains("trust_source=local_override"), "{stdout}");
    let json = run_winuxsh_with_env(&["plugin", "themes", "--json"], &envs);
    assert_success(&json, "plugin themes json");
    let stdout = stdout_text(&json);
    assert!(stdout.contains(r#""name": "testmarket""#), "{stdout}");
    assert!(stdout.contains(r#""source": "bundle""#), "{stdout}");
    assert!(
        stdout.contains(r#""trust_source": "local_override""#),
        "{stdout}"
    );
    assert!(stdout.contains(r#""pack": "themes""#), "{stdout}");
    let _ = fs::remove_dir_all(temp);
}
#[test]
fn plugin_themes_survive_missing_optional_legacy_pack() {
    let temp = temp_dir("plugin-themes-missing-legacy-pack");
    let bundle = temp.join("bundle");
    write_theme_test_bundle(&bundle, "9.9.11");
    let manifest_path = bundle.join("bundle.toml");
    let manifest = fs::read_to_string(&manifest_path).unwrap().replace(
        "available = [\"themes\"]",
        "available = [\"themes\", \"env-sync\"]",
    );
    fs::write(manifest_path, manifest).unwrap();

    let output = run_winuxsh_with_env(
        &["plugin", "themes"],
        &[("WINUXSH_PLUGIN_BUNDLE_PATH", bundle)],
    );
    assert_success(&output, "plugin themes with missing legacy pack");
    assert!(
        stdout_text(&output).contains("testmarket source=bundle"),
        "{}",
        stdout_text(&output)
    );
    let _ = fs::remove_dir_all(temp);
}
#[test]
fn plugin_themes_marks_external_bundle_trust_source() {
    let temp = temp_dir("plugin-themes-external-catalog");
    let bundle = temp.join("bundle");
    write_external_theme_test_bundle(&bundle, "9.9.10");
    let envs = [("WINUXSH_PLUGIN_BUNDLE_PATH", bundle)];
    let text = run_winuxsh_with_env(&["plugin", "themes"], &envs);
    assert_success(&text, "plugin themes external text");
    let stdout = stdout_text(&text);
    assert!(stdout.contains("community-tools@9.9.10"), "{stdout}");
    assert!(stdout.contains("trust_source=external_bundle"), "{stdout}");
    let json = run_winuxsh_with_env(&["plugin", "themes", "--json"], &envs);
    assert_success(&json, "plugin themes external json");
    let stdout = stdout_text(&json);
    assert!(
        stdout.contains(r#""bundle": "community-tools""#),
        "{stdout}"
    );
    assert!(
        stdout.contains(r#""trust_source": "external_bundle""#),
        "{stdout}"
    );
    let _ = fs::remove_dir_all(temp);
}
#[test]
fn plugin_info_text_describes_one_pack() {
    let output = run_winuxsh(&["plugin", "info", "git"]);
    assert_success(&output, "plugin info text");
    let stdout = stdout_text(&output);
    assert!(stdout.contains("Plugin: git"), "{stdout}");
    assert!(stdout.contains("Bundle: oh-my-winuxsh"), "{stdout}");
    assert!(stdout.contains("Execution model: host_builtin"), "{stdout}");
    assert!(
        stdout.contains("Externalization class: mixed_declarative_native"),
        "{stdout}"
    );
    assert!(
        stdout.contains("Permissions: cwd:read,process:run:git"),
        "{stdout}"
    );
    assert!(stdout.contains("Required binaries: git"), "{stdout}");
    assert!(stdout.contains("  aliases: yes"), "{stdout}");
    assert!(stdout.contains("  completions: git"), "{stdout}");
}
#[test]
fn plugin_info_json_describes_one_pack() {
    let output = run_winuxsh(&["plugin", "info", "zoxide", "--json"]);
    assert_success(&output, "plugin info json");
    let stdout = stdout_text(&output);
    assert!(stdout.contains(r#""name": "zoxide""#), "{stdout}");
    assert!(stdout.contains(r#""category": "workflow""#), "{stdout}");
    assert!(
        stdout.contains(r#""source": "compiled_fallback""#),
        "{stdout}"
    );
    assert!(
        stdout.contains(r#""trust_source": "official_bundle""#),
        "{stdout}"
    );
    assert!(
        stdout.contains(r#""execution_model": "host_builtin""#),
        "{stdout}"
    );
    assert!(
        stdout.contains(r#""externalization_class": "shell_effect_candidate""#),
        "{stdout}"
    );
    assert!(stdout.contains(r#""commands": ["#), "{stdout}");
    assert!(stdout.contains(r#""z""#), "{stdout}");
}
#[test]
fn plugin_info_marks_command_not_found_provider_candidate() {
    let text = run_winuxsh(&["plugin", "info", "command-not-found"]);
    assert_success(&text, "plugin info command-not-found text");
    let stdout = stdout_text(&text);
    assert!(stdout.contains("Plugin: command-not-found"), "{stdout}");
    assert!(
        stdout.contains("Externalization class: pure_provider_candidate"),
        "{stdout}"
    );
    assert!(
        stdout.contains("Target runtime: process_provider_available_builtin_until_migration"),
        "{stdout}"
    );
    assert!(
        stdout.contains("Missing host API/decision: bundle_migration_decision,provider_abi"),
        "{stdout}"
    );
    assert!(stdout.contains("Shell-mutating: no"), "{stdout}");
    assert!(stdout.contains("Fallback needed: yes"), "{stdout}");
    assert!(
        stdout.contains("  providers: command-not-found"),
        "{stdout}"
    );

    let json = run_winuxsh(&["plugin", "info", "command-not-found", "--json"]);
    assert_success(&json, "plugin info command-not-found json");
    let stdout = stdout_text(&json);
    assert!(stdout.contains(r#""providers": ["#), "{stdout}");
    assert!(stdout.contains(r#""command-not-found""#), "{stdout}");
}
#[test]
fn plugin_review_marks_command_not_found_process_provider_readiness() {
    let output = run_winuxsh(&["plugin", "review", "command-not-found"]);
    assert_success(&output, "plugin review command-not-found text");
    let stdout = stdout_text(&output);
    assert!(stdout.contains("Execution model: host_builtin"), "{stdout}");
    assert!(
        stdout.contains("Externalization class: pure_provider_candidate"),
        "{stdout}"
    );
    assert!(
        stdout.contains("Target runtime: process_provider_available_builtin_until_migration"),
        "{stdout}"
    );
    assert!(stdout.contains("Shell-mutating: no"), "{stdout}");
    assert!(
        stdout.contains("process provider binding exists for command-not-found"),
        "{stdout}"
    );
    assert!(
        stdout.contains("stable provider ABI is finalized"),
        "{stdout}"
    );
}
#[test]
fn plugin_info_unknown_pack_fails() {
    let output = run_winuxsh(&["plugin", "info", "unknown-pack"]);
    assert!(
        !output.status.success(),
        "unknown plugin should fail\nstdout={}\nstderr={}",
        stdout_text(&output),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("unknown plugin 'unknown-pack'"),
        "stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
}
#[test]
fn plugin_bundle_status_text_reports_compiled_fallback() {
    let temp = temp_dir("plugin-bundle-status-fallback");
    let output = run_winuxsh_with_env(
        &["plugin", "bundle", "status"],
        &[
            ("WINUXSH_PLUGIN_BUNDLE_PATH", temp.join("missing")),
            ("WINUXSH_PLUGIN_BUNDLE_ROOT", temp.join("root")),
            ("WINUXSH_PLUGIN_LOCK", temp.join("plugin-lock.toml")),
        ],
    );
    assert_success(&output, "plugin bundle status");
    let stdout = stdout_text(&output);
    assert!(
        stdout.contains("Official Winuxsh plugin bundle"),
        "{stdout}"
    );
    assert!(stdout.contains("State: compiled_fallback"), "{stdout}");
    assert!(stdout.contains("Bundle: oh-my-winuxsh"), "{stdout}");
    assert!(stdout.contains("Source: compiled_fallback"), "{stdout}");
    assert!(stdout.contains("Trust source: official_bundle"), "{stdout}");
    let _ = fs::remove_dir_all(temp);
}
#[test]
fn plugin_bundle_status_text_reports_app_bundled_baseline() {
    let temp = temp_dir("plugin-bundle-status-app-baseline");
    let bundle = temp.join("app").join("bundles").join("oh-my-winuxsh");
    write_minimal_test_bundle(&bundle, "9.9.8", "App-bundled Git aliases");
    let output = run_winuxsh_with_env(
        &["plugin", "bundle", "status"],
        &[("WINUXSH_APP_BUNDLE_PATH", bundle.clone())],
    );
    assert_success(&output, "plugin bundle status app baseline");
    let stdout = stdout_text(&output);
    assert!(stdout.contains("State: installed"), "{stdout}");
    assert!(stdout.contains("Bundle: oh-my-winuxsh"), "{stdout}");
    assert!(stdout.contains("Source: app_bundle"), "{stdout}");
    assert!(stdout.contains("Trust source: official_bundle"), "{stdout}");
    assert!(stdout.contains("Active version: 9.9.8"), "{stdout}");
    let info = run_winuxsh_with_env(
        &["plugin", "info", "git"],
        &[("WINUXSH_APP_BUNDLE_PATH", bundle)],
    );
    assert_success(&info, "plugin info app baseline");
    let stdout = stdout_text(&info);
    assert!(
        stdout.contains("Summary: App-bundled Git aliases"),
        "{stdout}"
    );
    let _ = fs::remove_dir_all(temp);
}
#[test]
fn plugin_bundle_status_marks_external_bundle_review_only() {
    let temp = temp_dir("plugin-bundle-status-external");
    let bundle = temp.join("bundle");
    write_external_process_test_bundle(&bundle, "9.9.7");
    let output = run_winuxsh_with_env(
        &["plugin", "bundle", "status"],
        &[("WINUXSH_PLUGIN_BUNDLE_PATH", bundle.clone())],
    );
    assert_success(&output, "plugin bundle status external bundle");
    let stdout = stdout_text(&output);
    assert!(stdout.contains("Winuxsh plugin bundle status"), "{stdout}");
    assert!(stdout.contains("State: installed"), "{stdout}");
    assert!(stdout.contains("Bundle: community-tools"), "{stdout}");
    assert!(stdout.contains("Source: env_override"), "{stdout}");
    assert!(stdout.contains("Trust source: external_bundle"), "{stdout}");
    assert!(
        stdout.contains("Message: using external bundle override (review-only)"),
        "{stdout}"
    );
    let json = run_winuxsh_with_env(
        &["plugin", "bundle", "status", "--json"],
        &[("WINUXSH_PLUGIN_BUNDLE_PATH", bundle.clone())],
    );
    assert_success(&json, "plugin bundle status external bundle json");
    let stdout = stdout_text(&json);
    assert!(
        stdout.contains(r#""bundle": "community-tools""#),
        "{stdout}"
    );
    assert!(
        stdout.contains(r#""trust_source": "external_bundle""#),
        "{stdout}"
    );
    assert!(
        stdout.contains(r#""message": "using external bundle override (review-only)""#),
        "{stdout}"
    );
    let _ = fs::remove_dir_all(temp);
}
#[test]
fn plugin_info_reads_installed_bundle_manifest() {
    let temp = temp_dir("plugin-installed-bundle-info");
    let bundle = temp.join("bundle");
    write_minimal_test_bundle(&bundle, "9.9.9", "Installed Git aliases");
    let output = run_winuxsh_with_env(
        &["plugin", "info", "git"],
        &[("WINUXSH_PLUGIN_BUNDLE_PATH", bundle.clone())],
    );
    assert_success(&output, "plugin info installed bundle");
    let stdout = stdout_text(&output);
    assert!(stdout.contains("Version: 9.9.9"), "{stdout}");
    assert!(
        stdout.contains("Summary: Installed Git aliases"),
        "{stdout}"
    );
    let status = run_winuxsh_with_env(
        &["plugin", "bundle", "status", "--json"],
        &[("WINUXSH_PLUGIN_BUNDLE_PATH", bundle)],
    );
    assert_success(&status, "plugin bundle status json");
    let stdout = stdout_text(&status);
    assert!(stdout.contains(r#""state": "installed""#), "{stdout}");
    assert!(stdout.contains(r#""bundle": "oh-my-winuxsh""#), "{stdout}");
    assert!(stdout.contains(r#""source": "env_override""#), "{stdout}");
    assert!(
        stdout.contains(r#""trust_source": "local_override""#),
        "{stdout}"
    );
    assert!(
        stdout.contains(r#""message": "using local override bundle""#),
        "{stdout}"
    );
    let _ = fs::remove_dir_all(temp);
}
#[test]
fn plugin_info_reads_installed_keybinding_metadata() {
    let temp = temp_dir("plugin-installed-keybinding-info");
    let bundle = temp.join("bundle");
    write_keybindings_test_bundle(&bundle, "9.9.6");
    let output = run_winuxsh_with_env(
        &["plugin", "info", "keybindings"],
        &[("WINUXSH_PLUGIN_BUNDLE_PATH", bundle)],
    );
    assert_success(&output, "plugin info installed keybindings");
    let stdout = stdout_text(&output);
    assert!(stdout.contains("Plugin: keybindings"), "{stdout}");
    assert!(stdout.contains("Keybinding metadata:"), "{stdout}");
    assert!(
        stdout.contains("common keymap=all bindings=2 summary=Bundle common keybindings v1"),
        "{stdout}"
    );
    assert!(
        stdout.contains("emacs keymap=emacs bindings=1 summary=Bundle emacs keybindings v1"),
        "{stdout}"
    );
    assert!(
        stdout.contains("vi keymap=vi bindings=1 summary=Bundle vi keybindings v1"),
        "{stdout}"
    );
    let _ = fs::remove_dir_all(temp);
}
#[test]
fn plugin_info_reads_installed_process_pack_contract() {
    let temp = temp_dir("plugin-installed-process-info");
    let bundle = temp.join("bundle");
    write_process_test_bundle(&bundle, "9.9.7");
    let output = run_winuxsh_with_env(
        &["plugin", "info", "process-echo"],
        &[("WINUXSH_PLUGIN_BUNDLE_PATH", bundle.clone())],
    );
    assert_success(&output, "plugin info installed process pack");
    let stdout = stdout_text(&output);
    assert!(stdout.contains("Plugin: process-echo"), "{stdout}");
    assert!(stdout.contains("Kind: process"), "{stdout}");
    assert!(stdout.contains("Default: off"), "{stdout}");
    assert!(
        stdout.contains("Permissions: cwd:read,process:run:winuxsh-process-echo"),
        "{stdout}"
    );
    assert!(stdout.contains("Process:"), "{stdout}");
    assert!(
        stdout.contains("  protocol: winuxsh:process-plugin@0.1.0"),
        "{stdout}"
    );
    assert!(
        stdout.contains("  command: winuxsh-process-echo"),
        "{stdout}"
    );
    assert!(stdout.contains("  args: --format,json"), "{stdout}");
    assert!(stdout.contains("  timeout_millis: 1000"), "{stdout}");
    assert!(stdout.contains("  commands: process-echo"), "{stdout}");
    let _ = fs::remove_dir_all(temp);
}
#[test]
fn plugin_info_reads_installed_source_pack_contract() {
    let temp = temp_dir("plugin-installed-source-info");
    let bundle = temp.join("bundle");
    write_source_test_bundle(&bundle, "9.9.7", "init.winux");
    let output = run_winuxsh_with_env(
        &["plugin", "info", "source-test"],
        &[("WINUXSH_PLUGIN_BUNDLE_PATH", bundle.clone())],
    );
    assert_success(&output, "plugin info installed source pack");
    let stdout = stdout_text(&output);
    assert!(stdout.contains("Plugin: source-test"), "{stdout}");
    assert!(stdout.contains("Kind: source"), "{stdout}");
    assert!(stdout.contains("Execution model: shell_source"), "{stdout}");
    assert!(stdout.contains("Permissions: shell:source"), "{stdout}");
    assert!(stdout.contains("Source:"), "{stdout}");
    assert!(
        stdout.contains("  entry: packs/source-test/init.winux"),
        "{stdout}"
    );
    let _ = fs::remove_dir_all(temp);
}
#[test]
fn plugin_inventory_reads_framework_plugin_directories() {
    let temp = temp_dir("plugin-framework-directories");
    let bundle = temp.join("bundle");
    write_framework_directory_test_bundle(&bundle, "9.9.11");

    let list = run_winuxsh_with_env(
        &["plugin", "list", "--json"],
        &[("WINUXSH_PLUGIN_BUNDLE_PATH", bundle.clone())],
    );
    assert_success(&list, "plugin list framework directories json");
    let stdout = stdout_text(&list);
    assert!(stdout.contains(r#""name": "prompt-core""#), "{stdout}");
    assert!(stdout.contains(r#""name": "theme-minimal""#), "{stdout}");
    assert!(stdout.contains(r#""name": "keybindings""#), "{stdout}");
    assert!(stdout.contains(r#""kind": "bridge""#), "{stdout}");
    assert!(
        !stdout.contains("Legacy Git aliases"),
        "framework git plugin should replace legacy pack\n{stdout}"
    );

    let git = run_winuxsh_with_env(
        &["plugin", "info", "git"],
        &[("WINUXSH_PLUGIN_BUNDLE_PATH", bundle.clone())],
    );
    assert_success(&git, "plugin info framework git");
    let stdout = stdout_text(&git);
    assert!(stdout.contains("Kind: source"), "{stdout}");
    assert!(stdout.contains("Summary: Framework Git plugin"), "{stdout}");
    assert!(
        stdout.contains("  entry: plugins/git/git.plugin.winux"),
        "{stdout}"
    );

    let keybindings = run_winuxsh_with_env(
        &["plugin", "info", "keybindings"],
        &[("WINUXSH_PLUGIN_BUNDLE_PATH", bundle)],
    );
    assert_success(&keybindings, "plugin info framework keybindings bridge");
    let stdout = stdout_text(&keybindings);
    assert!(stdout.contains("Kind: bridge"), "{stdout}");
    assert!(stdout.contains("Execution model: host_bridge"), "{stdout}");
    let _ = fs::remove_dir_all(temp);
}
#[test]
fn plugin_review_text_describes_process_permissions_before_enable() {
    let temp = temp_dir("plugin-review-process-text");
    let bundle = temp.join("bundle");
    let empty_bin = temp.join("empty-bin");
    fs::create_dir_all(&empty_bin).unwrap();
    write_process_test_bundle(&bundle, "9.9.7");
    let mut command = base_winuxsh_command(&["plugin", "review", "process-echo"]);
    command
        .env("WINUXSH_PLUGIN_BUNDLE_PATH", &bundle)
        .env("PATH", &empty_bin);
    let output = command.output().expect("failed to run plugin review");
    assert_success(&output, "plugin review process text");
    let stdout = stdout_text(&output);
    assert!(
        stdout.contains("Plugin permission review: process-echo"),
        "{stdout}"
    );
    assert!(stdout.contains("Kind: process"), "{stdout}");
    assert!(stdout.contains("Trust source: local_override"), "{stdout}");
    assert!(
        stdout.contains("process:run:winuxsh-process-echo risk=high scope=process"),
        "{stdout}"
    );
    assert!(
        stdout.contains("May execute the native command 'winuxsh-process-echo'."),
        "{stdout}"
    );
    assert!(
        stdout.contains("missing required binaries: winuxsh-process-echo"),
        "{stdout}"
    );
    assert!(
        stdout.contains("Install command: winuxsh plugin install process-echo"),
        "{stdout}"
    );
    let _ = fs::remove_dir_all(temp);
}
#[test]
fn plugin_review_text_marks_compiled_official_trust_source() {
    let output = run_winuxsh(&["plugin", "review", "git"]);
    assert_success(&output, "plugin review compiled official text");
    let stdout = stdout_text(&output);
    assert!(stdout.contains("Plugin permission review: git"), "{stdout}");
    assert!(stdout.contains("Execution model: host_builtin"), "{stdout}");
    assert!(
        stdout.contains("Externalization class: mixed_declarative_native"),
        "{stdout}"
    );
    assert!(stdout.contains("Trust source: official_bundle"), "{stdout}");
    assert!(
        stdout.contains("Mixed pack: static bundle assets are declarative"),
        "{stdout}"
    );
}
#[test]
fn plugin_review_json_marks_external_bundle_trust_source() {
    let temp = temp_dir("plugin-review-external-bundle-json");
    let bundle = temp.join("bundle");
    write_external_process_test_bundle(&bundle, "9.9.7");
    let output = run_winuxsh_with_env(
        &["plugin", "review", "process-echo", "--json"],
        &[("WINUXSH_PLUGIN_BUNDLE_PATH", bundle.clone())],
    );
    assert_success(&output, "plugin review external bundle json");
    let stdout = stdout_text(&output);
    assert!(stdout.contains(r#""plugin": "process-echo""#), "{stdout}");
    assert!(
        stdout.contains(r#""trust_source": "external_bundle""#),
        "{stdout}"
    );
    assert!(
        stdout.contains(r#""token": "process:run:winuxsh-process-echo""#),
        "{stdout}"
    );
    assert!(
        stdout.contains(
            r#""install_command": "unavailable: external bundle install requires registry trust policy""#
        ),
        "{stdout}"
    );
    assert!(
        stdout.contains(
            "External bundle install is review-only until registry trust policy is implemented."
        ),
        "{stdout}"
    );
    let _ = fs::remove_dir_all(temp);
}
#[test]
fn plugin_review_json_uses_bundle_trust_source_over_pack_claim() {
    let temp = temp_dir("plugin-review-spoofed-bundle-json");
    let bundle = temp.join("bundle");
    write_external_process_test_bundle_with_pack_bundle(&bundle, "9.9.7", "oh-my-winuxsh");
    let output = run_winuxsh_with_env(
        &["plugin", "review", "process-echo", "--json"],
        &[("WINUXSH_PLUGIN_BUNDLE_PATH", bundle.clone())],
    );
    assert_success(&output, "plugin review spoofed external bundle json");
    let stdout = stdout_text(&output);
    assert!(stdout.contains(r#""plugin": "process-echo""#), "{stdout}");
    assert!(
        stdout.contains(r#""trust_source": "external_bundle""#),
        "{stdout}"
    );
    let _ = fs::remove_dir_all(temp);
}
#[test]
fn plugin_update_and_rollback_switch_active_bundle_from_cli() {
    let temp = temp_dir("plugin-update-rollback-cli");
    let bundle_v1 = temp.join("bundle-v1");
    let bundle_v2 = temp.join("bundle-v2");
    let envs = plugin_bundle_env(&temp);
    write_minimal_test_bundle(&bundle_v1, "9.9.1", "Git aliases v1");
    write_minimal_test_bundle(&bundle_v2, "9.9.2", "Git aliases v2");
    let update_v1 = run_winuxsh_with_env_owned(
        &[
            "plugin".to_string(),
            "update".to_string(),
            "oh-my-winuxsh".to_string(),
            "--from".to_string(),
            bundle_v1.display().to_string(),
        ],
        &envs,
    );
    assert_success(&update_v1, "plugin update v1");
    let stdout = stdout_text(&update_v1);
    assert!(
        stdout.contains("Updated bundle 'oh-my-winuxsh' to 9.9.1"),
        "{stdout}"
    );
    let update_v2 = run_winuxsh_with_env_owned(
        &[
            "plugin".to_string(),
            "update".to_string(),
            "oh-my-winuxsh".to_string(),
            "--from".to_string(),
            bundle_v2.display().to_string(),
            "--json".to_string(),
        ],
        &envs,
    );
    assert_success(&update_v2, "plugin update v2 json");
    let stdout = stdout_text(&update_v2);
    assert!(stdout.contains(r#""version": "9.9.2""#), "{stdout}");
    assert!(stdout.contains(r#""previous_path""#), "{stdout}");
    let rollback = run_winuxsh_with_env(&["plugin", "rollback", "oh-my-winuxsh"], &envs);
    assert_success(&rollback, "plugin rollback");
    let stdout = stdout_text(&rollback);
    assert!(
        stdout.contains("Rolled back bundle 'oh-my-winuxsh' to 9.9.1"),
        "{stdout}"
    );
    let status = run_winuxsh_with_env(&["plugin", "bundle", "status"], &envs);
    assert_success(&status, "plugin bundle status after rollback");
    let stdout = stdout_text(&status);
    assert!(stdout.contains("Active version: 9.9.1"), "{stdout}");
    let _ = fs::remove_dir_all(temp);
}
#[test]
fn plugin_update_accepts_source_bundle_with_winux_entry() {
    let temp = temp_dir("plugin-update-source-winux");
    let bundle = temp.join("bundle");
    let envs = plugin_bundle_env(&temp);
    write_source_test_bundle(&bundle, "9.9.21", "init.winux");
    let update = run_winuxsh_with_env_owned(
        &[
            "plugin".to_string(),
            "update".to_string(),
            "oh-my-winuxsh".to_string(),
            "--from".to_string(),
            bundle.display().to_string(),
        ],
        &envs,
    );
    assert_success(&update, "plugin update source bundle");
    let stdout = stdout_text(&update);
    assert!(
        stdout.contains("Updated bundle 'oh-my-winuxsh' to 9.9.21"),
        "{stdout}"
    );
    let info = run_winuxsh_with_env(&["plugin", "info", "source-test"], &envs);
    assert_success(&info, "plugin info after source bundle update");
    let stdout = stdout_text(&info);
    assert!(stdout.contains("Kind: source"), "{stdout}");
    assert!(
        stdout.contains("  entry: packs/source-test/init.winux"),
        "{stdout}"
    );
    let _ = fs::remove_dir_all(temp);
}
#[test]
fn plugin_update_rejects_source_bundle_without_winux_entry() {
    let temp = temp_dir("plugin-update-source-bad-suffix");
    let bundle = temp.join("bundle");
    let envs = plugin_bundle_env(&temp);
    write_source_test_bundle(&bundle, "9.9.22", "init.winsh");
    let update = run_winuxsh_with_env_owned(
        &[
            "plugin".to_string(),
            "update".to_string(),
            "oh-my-winuxsh".to_string(),
            "--from".to_string(),
            bundle.display().to_string(),
        ],
        &envs,
    );
    assert!(
        !update.status.success(),
        "bad source suffix update should fail\nstdout={}\nstderr={}",
        stdout_text(&update),
        String::from_utf8_lossy(&update.stderr)
    );
    let stderr = String::from_utf8_lossy(&update.stderr);
    assert!(stderr.contains("must end in .winux"), "stderr={stderr}");
    let status = run_winuxsh_with_env(&["plugin", "bundle", "status"], &envs);
    assert_success(&status, "plugin bundle status after bad source suffix");
    let stdout = stdout_text(&status);
    assert!(stdout.contains("State: compiled_fallback"), "{stdout}");
    let _ = fs::remove_dir_all(temp);
}
#[test]
fn plugin_update_installs_zip_archive_from_cli() {
    let temp = temp_dir("plugin-update-zip-cli");
    let bundle = temp.join("bundle");
    let archive = temp.join("oh-my-winuxsh-9.9.3.zip");
    let envs = plugin_bundle_env(&temp);
    write_minimal_test_bundle(&bundle, "9.9.3", "Git aliases zipped");
    write_bundle_zip_from_dir(&bundle, &archive);
    let archive_checksum = test_file_sha256(&archive);
    let update = run_winuxsh_with_env_owned(
        &[
            "plugin".to_string(),
            "update".to_string(),
            "oh-my-winuxsh".to_string(),
            "--from".to_string(),
            archive.display().to_string(),
            "--checksum".to_string(),
            archive_checksum,
        ],
        &envs,
    );
    assert_success(&update, "plugin update zip");
    let stdout = stdout_text(&update);
    assert!(
        stdout.contains("Updated bundle 'oh-my-winuxsh' to 9.9.3"),
        "{stdout}"
    );
    assert!(stdout.contains("SHA-256:"), "{stdout}");
    let info = run_winuxsh_with_env(&["plugin", "info", "git"], &envs);
    assert_success(&info, "plugin info after zip update");
    let stdout = stdout_text(&info);
    assert!(stdout.contains("Version: 9.9.3"), "{stdout}");
    assert!(stdout.contains("Summary: Git aliases zipped"), "{stdout}");
    let alias = run_winuxsh_with_env(&["-c", "alias gphase"], &envs);
    assert_success(&alias, "bundle alias visible after zip update");
    let stdout = stdout_text(&alias);
    assert!(
        stdout.contains("gphase='git status --phase-six'"),
        "{stdout}"
    );
    let _ = fs::remove_dir_all(temp);
}
#[test]
fn plugin_update_rejects_zip_without_required_checksum_from_index() {
    let temp = temp_dir("plugin-update-zip-requires-checksum");
    let bundle = temp.join("bundle");
    let archive = temp.join("oh-my-winuxsh-9.9.31.zip");
    let envs = plugin_bundle_env(&temp);
    write_minimal_test_bundle(&bundle, "9.9.31", "Git aliases checksum required");
    write_bundle_zip_from_dir(&bundle, &archive);
    let update = run_winuxsh_with_env_owned(
        &[
            "plugin".to_string(),
            "update".to_string(),
            "oh-my-winuxsh".to_string(),
            "--from".to_string(),
            archive.display().to_string(),
        ],
        &envs,
    );
    assert!(
        !update.status.success(),
        "zip update without checksum should fail\nstdout={}\nstderr={}",
        stdout_text(&update),
        String::from_utf8_lossy(&update.stderr)
    );
    let stderr = String::from_utf8_lossy(&update.stderr);
    assert!(
        stderr.contains("requires checksum verification"),
        "stderr={stderr}"
    );
    let leftovers = staging_dirs(&temp.join("root").join("oh-my-winuxsh"));
    assert!(
        leftovers.is_empty(),
        "checksum rejection should clean staging dirs: {leftovers:?}"
    );
    let status = run_winuxsh_with_env(&["plugin", "bundle", "status"], &envs);
    assert_success(&status, "plugin bundle status after missing checksum");
    let stdout = stdout_text(&status);
    assert!(stdout.contains("State: compiled_fallback"), "{stdout}");
    let _ = fs::remove_dir_all(temp);
}
#[test]
fn plugin_update_rejects_index_manifest_drift() {
    let temp = temp_dir("plugin-update-index-drift");
    let bundle = temp.join("bundle");
    let envs = plugin_bundle_env(&temp);
    write_minimal_test_bundle(&bundle, "9.9.32", "Git aliases index drift");
    let index_path = bundle.join("index.toml");
    let index = fs::read_to_string(&index_path).unwrap().replace(
        "summary = \"Git aliases index drift\"",
        "summary = \"Drifted index summary\"",
    );
    fs::write(&index_path, index).unwrap();
    let update = run_winuxsh_with_env_owned(
        &[
            "plugin".to_string(),
            "update".to_string(),
            "oh-my-winuxsh".to_string(),
            "--from".to_string(),
            bundle.display().to_string(),
        ],
        &envs,
    );
    assert!(
        !update.status.success(),
        "index drift update should fail\nstdout={}\nstderr={}",
        stdout_text(&update),
        String::from_utf8_lossy(&update.stderr)
    );
    let stderr = String::from_utf8_lossy(&update.stderr);
    assert!(
        stderr.contains("index.toml git.summary must match manifest"),
        "stderr={stderr}"
    );
    let status = run_winuxsh_with_env(&["plugin", "bundle", "status"], &envs);
    assert_success(&status, "plugin bundle status after index drift");
    let stdout = stdout_text(&status);
    assert!(stdout.contains("State: compiled_fallback"), "{stdout}");
    let _ = fs::remove_dir_all(temp);
}
#[test]
fn plugin_update_rejects_bundle_requiring_newer_winuxsh_from_cli() {
    let temp = temp_dir("plugin-update-newer-host-cli");
    let bundle = temp.join("bundle");
    let envs = plugin_bundle_env(&temp);
    write_minimal_test_bundle(&bundle, "9.9.4", "Git aliases future host");
    let bundle_toml_path = bundle.join("bundle.toml");
    let bundle_toml = fs::read_to_string(&bundle_toml_path)
        .unwrap()
        .replace("min_winuxsh = \"0.8.3\"", "min_winuxsh = \"999.0.0\"");
    fs::write(&bundle_toml_path, bundle_toml).unwrap();
    let update = run_winuxsh_with_env_owned(
        &[
            "plugin".to_string(),
            "update".to_string(),
            "oh-my-winuxsh".to_string(),
            "--from".to_string(),
            bundle.display().to_string(),
        ],
        &envs,
    );
    assert!(
        !update.status.success(),
        "newer-host bundle update should fail\nstdout={}\nstderr={}",
        stdout_text(&update),
        String::from_utf8_lossy(&update.stderr)
    );
    let stderr = String::from_utf8_lossy(&update.stderr);
    assert!(
        stderr.contains("requires Winuxsh >= 999.0.0"),
        "stderr={stderr}"
    );
    let status = run_winuxsh_with_env(&["plugin", "bundle", "status"], &envs);
    assert_success(&status, "plugin bundle status after incompatible update");
    let stdout = stdout_text(&status);
    assert!(stdout.contains("State: compiled_fallback"), "{stdout}");
    let _ = fs::remove_dir_all(temp);
}
#[test]
fn plugin_update_checksum_mismatch_leaves_active_bundle_from_cli() {
    let temp = temp_dir("plugin-update-checksum-cli");
    let bundle_v1 = temp.join("bundle-v1");
    let invalid_archive = temp.join("bad.zip");
    let envs = plugin_bundle_env(&temp);
    write_minimal_test_bundle(&bundle_v1, "9.9.1", "Git aliases v1");
    fs::write(&invalid_archive, b"not a bundle release").unwrap();
    let update_v1 = run_winuxsh_with_env_owned(
        &[
            "plugin".to_string(),
            "update".to_string(),
            "oh-my-winuxsh".to_string(),
            "--from".to_string(),
            bundle_v1.display().to_string(),
        ],
        &envs,
    );
    assert_success(&update_v1, "plugin update v1 before checksum mismatch");
    let mismatch = run_winuxsh_with_env_owned(
        &[
            "plugin".to_string(),
            "update".to_string(),
            "oh-my-winuxsh".to_string(),
            "--from".to_string(),
            invalid_archive.display().to_string(),
            "--checksum".to_string(),
            "0000".to_string(),
        ],
        &envs,
    );
    assert!(
        !mismatch.status.success(),
        "checksum mismatch should fail\nstdout={}\nstderr={}",
        stdout_text(&mismatch),
        String::from_utf8_lossy(&mismatch.stderr)
    );
    assert!(
        String::from_utf8_lossy(&mismatch.stderr).contains("checksum mismatch"),
        "stderr={}",
        String::from_utf8_lossy(&mismatch.stderr)
    );
    let status = run_winuxsh_with_env(&["plugin", "bundle", "status"], &envs);
    assert_success(&status, "plugin bundle status after checksum mismatch");
    let stdout = stdout_text(&status);
    assert!(stdout.contains("Active version: 9.9.1"), "{stdout}");
    let _ = fs::remove_dir_all(temp);
}
#[test]
fn plugin_update_rejects_invalid_process_pack_contract() {
    let temp = temp_dir("plugin-process-update-invalid");
    let bundle = temp.join("bundle");
    let envs = plugin_bundle_env(&temp);
    write_process_test_bundle(&bundle, "9.9.8");
    let manifest_path = bundle
        .join("packs")
        .join("process-echo")
        .join("plugin.toml");
    let manifest = fs::read_to_string(&manifest_path)
        .unwrap()
        .replace("default = false", "default = true");
    fs::write(&manifest_path, manifest).unwrap();
    let update = run_winuxsh_with_env_owned(
        &[
            "plugin".to_string(),
            "update".to_string(),
            "oh-my-winuxsh".to_string(),
            "--from".to_string(),
            bundle.display().to_string(),
        ],
        &envs,
    );
    assert!(
        !update.status.success(),
        "invalid process pack should fail update\nstdout={}\nstderr={}",
        stdout_text(&update),
        String::from_utf8_lossy(&update.stderr)
    );
    assert!(
        String::from_utf8_lossy(&update.stderr)
            .contains("process pack 'process-echo' must be explicit opt-in"),
        "stderr={}",
        String::from_utf8_lossy(&update.stderr)
    );
    let _ = fs::remove_dir_all(temp);
}
#[test]
fn plugin_update_rejects_unknown_provider_export() {
    let temp = temp_dir("plugin-update-unknown-provider");
    let bundle = temp.join("bundle");
    let envs = plugin_bundle_env(&temp);
    write_minimal_test_bundle(&bundle, "9.9.33", "Git aliases unknown provider");
    let manifest_path = bundle.join("packs").join("git").join("plugin.toml");
    let manifest = fs::read_to_string(&manifest_path).unwrap().replace(
        "keybindings = []",
        "keybindings = []\nproviders = [\"prompt\"]",
    );
    fs::write(&manifest_path, manifest).unwrap();
    let update = run_winuxsh_with_env_owned(
        &[
            "plugin".to_string(),
            "update".to_string(),
            "oh-my-winuxsh".to_string(),
            "--from".to_string(),
            bundle.display().to_string(),
        ],
        &envs,
    );
    assert!(
        !update.status.success(),
        "unknown provider export should fail update\nstdout={}\nstderr={}",
        stdout_text(&update),
        String::from_utf8_lossy(&update.stderr)
    );
    assert!(
        String::from_utf8_lossy(&update.stderr)
            .contains("pack 'git' exports unknown provider 'prompt'"),
        "stderr={}",
        String::from_utf8_lossy(&update.stderr)
    );
    let _ = fs::remove_dir_all(temp);
}
#[test]
fn plugin_update_rejects_process_provider_without_command_diagnose_permission() {
    let temp = temp_dir("plugin-update-process-provider");
    let bundle = temp.join("bundle");
    let envs = plugin_bundle_env(&temp);
    write_process_test_bundle(&bundle, "9.9.34");
    let manifest_path = bundle
        .join("packs")
        .join("process-echo")
        .join("plugin.toml");
    let manifest = fs::read_to_string(&manifest_path).unwrap().replace(
        "keybindings = []",
        "keybindings = []\nproviders = [\"command-not-found\"]",
    );
    fs::write(&manifest_path, manifest).unwrap();
    let update = run_winuxsh_with_env_owned(
        &[
            "plugin".to_string(),
            "update".to_string(),
            "oh-my-winuxsh".to_string(),
            "--from".to_string(),
            bundle.display().to_string(),
        ],
        &envs,
    );
    assert!(
        !update.status.success(),
        "process provider without command diagnose should fail update\nstdout={}\nstderr={}",
        stdout_text(&update),
        String::from_utf8_lossy(&update.stderr)
    );
    let stderr = String::from_utf8_lossy(&update.stderr);
    assert!(stderr.contains("requires permission"), "stderr={stderr}");
    assert!(stderr.contains("command:diagnose"), "stderr={stderr}");
    let _ = fs::remove_dir_all(temp);
}

fn run_winuxsh(args: &[&str]) -> Output {
    run_winuxsh_with_env(args, &[])
}
fn run_winuxsh_with_env(args: &[&str], envs: &[(&str, PathBuf)]) -> Output {
    let mut command = base_winuxsh_command(args);
    for (key, value) in envs {
        command.env(key, value);
    }
    command
        .output()
        .unwrap_or_else(|err| panic!("failed to run winuxsh {args:?}: {err}"))
}
fn run_winuxsh_with_env_owned(args: &[String], envs: &[(&str, PathBuf)]) -> Output {
    let mut command = base_winuxsh_command(&[]);
    command.args(args);
    for (key, value) in envs {
        command.env(key, value);
    }
    command
        .output()
        .unwrap_or_else(|err| panic!("failed to run winuxsh {args:?}: {err}"))
}
fn base_winuxsh_command(args: &[&str]) -> Command {
    let no_bundle = std::env::temp_dir().join("winuxsh-plugin-tests-no-installed-bundle");
    let mut command = Command::new(winuxsh_binary());
    command
        .args(args)
        .env("WINUXSH_PLUGIN_BUNDLE_PATH", no_bundle.join("missing"))
        .env("WINUXSH_PLUGIN_BUNDLE_ROOT", no_bundle.join("root"))
        .env("WINUXSH_APP_BUNDLE_PATH", no_bundle.join("app-missing"))
        .env("WINUXSH_PLUGIN_LOCK", no_bundle.join("plugin-lock.toml"));
    command
}
fn assert_success(output: &Output, context: &str) {
    assert!(
        output.status.success(),
        "{context} failed: status={:?}\nstdout={}\nstderr={}",
        output.status.code(),
        stdout_text(output),
        String::from_utf8_lossy(&output.stderr)
    );
}
fn stdout_text(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).replace("\r\n", "\n")
}
fn temp_dir(name: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!("winuxsh-{name}-{}-{nanos}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}
fn plugin_bundle_env(temp: &Path) -> Vec<(&'static str, PathBuf)> {
    vec![
        ("WINUXSH_PLUGIN_BUNDLE_PATH", temp.join("missing")),
        ("WINUXSH_PLUGIN_BUNDLE_ROOT", temp.join("root")),
        ("WINUXSH_PLUGIN_LOCK", temp.join("plugin-lock.toml")),
    ]
}
fn staging_dirs(bundle_root: &Path) -> Vec<PathBuf> {
    if !bundle_root.exists() {
        return Vec::new();
    }
    let mut dirs = fs::read_dir(bundle_root)
        .unwrap()
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.is_dir()
                && path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with(".staging-"))
        })
        .collect::<Vec<_>>();
    dirs.sort();
    dirs
}
fn write_test_bundle_index(
    path: &Path,
    version: &str,
    pack_name: &str,
    kind: &str,
    category: &str,
    summary: &str,
    default_enabled: bool,
    permissions: &[&str],
    required_binaries: &[&str],
) {
    let artifact = format!("oh-my-winuxsh-{version}.zip");
    let text = format!(
        r#"schema = "winuxsh:plugin-index@0.1.0"
bundle = "oh-my-winuxsh"
version = {version:?}
bundle_api = "winuxsh:plugin-bundle@0.1.0"
min_winuxsh = "0.8.3"
[release]
artifact = {artifact:?}
checksum = "{artifact}.sha256"
checksum_algorithm = "sha256"
checksum_required = true
signature = "unsupported"
[[packs]]
name = {pack_name:?}
version = {version:?}
api = "winuxsh:plugin@0.1.0"
kind = {kind:?}
category = {category:?}
summary = {summary:?}
default = {default_enabled}
permissions = {permissions}
required_binaries = {required_binaries}
"#,
        permissions = toml_string_list(permissions),
        required_binaries = toml_string_list(required_binaries),
    );
    fs::write(path.join("index.toml"), text).unwrap();
}
fn toml_string_list(values: &[&str]) -> String {
    let entries = values
        .iter()
        .map(|value| format!("{value:?}"))
        .collect::<Vec<_>>()
        .join(", ");
    format!("[{entries}]")
}
fn write_minimal_test_bundle(path: &Path, version: &str, summary: &str) {
    fs::create_dir_all(path.join("packs").join("git")).unwrap();
    fs::write(
        path.join("bundle.toml"),
        format!(
            r#"name = "oh-my-winuxsh"
version = {version:?}
api = "winuxsh:plugin-bundle@0.1.0"
min_winuxsh = "0.8.3"
[packs]
default = ["git"]
available = ["git"]
[layout]
packs_dir = "packs"
aliases_dir = "aliases"
completions_dir = "completions"
prompts_dir = "prompts"
"#
        ),
    )
    .unwrap();
    fs::create_dir_all(path.join("aliases")).unwrap();
    fs::write(
        path.join("aliases").join("git.toml"),
        "[aliases]\ngst = \"git status\"\ngphase = \"git status --phase-six\"\n",
    )
    .unwrap();
    fs::create_dir_all(path.join("completions")).unwrap();
    fs::write(
        path.join("completions").join("git.toml"),
        r#"command = "git"
description = "test bundle git"
[[flags]]
long = "--bundle-only"
description = "flag loaded from test bundle"
[[subcommands]]
name = "bundle-subcommand"
description = "subcommand loaded from test bundle"
"#,
    )
    .unwrap();
    fs::create_dir_all(path.join("prompts")).unwrap();
    fs::write(
        path.join("prompts").join("segments.toml"),
        r#"[segments.git]
id = "vcs"
description = "test git prompt segment"
[segments.prompt_char]
id = "prompt_char"
description = "test prompt symbol"
[segments.newline]
id = "newline"
description = "test newline"
[presets.classic]
left = ["git", "newline", "prompt_char"]
right = []
separator = "|"
git_prompt_format = "bundle:{git_branch}"
"#,
    )
    .unwrap();
    fs::write(
        path.join("packs").join("git").join("plugin.toml"),
        format!(
            r#"name = "git"
bundle = "oh-my-winuxsh"
version = {version:?}
kind = "builtin"
api = "winuxsh:plugin@0.1.0"
category = "devtools"
summary = {summary:?}
default = true
permissions = ["cwd:read", "process:run:git"]
required_binaries = ["git"]
[exports]
aliases = true
completions = ["git"]
prompt_segments = ["git"]
hooks = []
commands = []
keybindings = []
"#
        ),
    )
    .unwrap();
    write_test_bundle_index(
        path,
        version,
        "git",
        "builtin",
        "devtools",
        summary,
        true,
        &["cwd:read", "process:run:git"],
        &["git"],
    );
}
fn write_framework_directory_test_bundle(path: &Path, version: &str) {
    write_minimal_test_bundle(path, version, "Legacy Git aliases");
    fs::create_dir_all(path.join("plugins").join("prompt-core")).unwrap();
    fs::create_dir_all(path.join("plugins").join("git")).unwrap();
    fs::create_dir_all(path.join("plugins").join("theme-minimal")).unwrap();
    fs::create_dir_all(path.join("plugins").join("keybindings")).unwrap();
    fs::create_dir_all(path.join("plugins").join("command-not-found")).unwrap();
    fs::create_dir_all(path.join("themes")).unwrap();

    fs::write(
        path.join("plugins").join("prompt-core").join("plugin.toml"),
        format!(
            r#"name = "prompt-core"
version = {version:?}
kind = "source"
entry = "prompt-core.plugin.winux"
summary = "Framework prompt core"
permissions = ["shell:source"]
required_binaries = []
[exports]
prompt_segments = ["cwd", "git", "prompt_char"]
"#
        ),
    )
    .unwrap();
    fs::write(
        path.join("plugins")
            .join("prompt-core")
            .join("prompt-core.plugin.winux"),
        "export FRAMEWORK_PROMPT_CORE=loaded\n",
    )
    .unwrap();

    fs::write(
        path.join("plugins").join("git").join("plugin.toml"),
        format!(
            r#"name = "git"
version = {version:?}
kind = "source"
entry = "git.plugin.winux"
summary = "Framework Git plugin"
permissions = ["shell:source", "cwd:read", "process:run:git"]
required_binaries = ["git"]
[exports]
aliases = true
completions = ["git"]
"#
        ),
    )
    .unwrap();
    fs::write(
        path.join("plugins").join("git").join("git.plugin.winux"),
        "alias gst='git status --framework'\n",
    )
    .unwrap();

    fs::write(
        path.join("plugins")
            .join("theme-minimal")
            .join("plugin.toml"),
        format!(
            r#"name = "theme-minimal"
version = {version:?}
kind = "source"
entry = "theme-minimal.plugin.winux"
summary = "Framework minimal theme"
permissions = ["shell:source"]
required_binaries = []
[exports]
themes = ["minimal"]
"#
        ),
    )
    .unwrap();
    fs::write(
        path.join("plugins")
            .join("theme-minimal")
            .join("theme-minimal.plugin.winux"),
        "export FRAMEWORK_THEME=minimal\n",
    )
    .unwrap();
    fs::write(
        path.join("themes").join("minimal.toml"),
        r#"[prompt_symbol]
fg = "light magenta"
"#,
    )
    .unwrap();

    fs::write(
        path.join("plugins").join("keybindings").join("plugin.toml"),
        format!(
            r#"name = "keybindings"
version = {version:?}
kind = "bridge"
entry = "keybindings.plugin.winux"
summary = "Framework keybinding bridge"
permissions = []
required_binaries = []
[exports]
keybindings = ["common"]
host_bridge = "reedline-keybindings"
"#
        ),
    )
    .unwrap();
    fs::write(
        path.join("plugins")
            .join("command-not-found")
            .join("plugin.toml"),
        format!(
            r#"name = "command-not-found"
version = {version:?}
kind = "bridge"
entry = "command-not-found.plugin.winux"
summary = "Framework command-not-found bridge"
permissions = ["command:diagnose"]
required_binaries = []
[exports]
providers = ["command-not-found"]
host_bridge = "command-not-found-provider"
"#
        ),
    )
    .unwrap();
}
fn write_theme_test_bundle(path: &Path, version: &str) {
    fs::create_dir_all(path.join("packs").join("themes")).unwrap();
    fs::create_dir_all(path.join("themes")).unwrap();
    fs::write(
        path.join("bundle.toml"),
        format!(
            r#"name = "oh-my-winuxsh"
version = {version:?}
api = "winuxsh:plugin-bundle@0.1.0"
min_winuxsh = "0.8.3"
[packs]
default = ["themes"]
available = ["themes"]
[layout]
packs_dir = "packs"
themes_dir = "themes"
"#
        ),
    )
    .unwrap();
    fs::write(
        path.join("packs").join("themes").join("plugin.toml"),
        format!(
            r#"name = "themes"
bundle = "oh-my-winuxsh"
version = {version:?}
kind = "builtin"
api = "winuxsh:plugin@0.1.0"
category = "ux"
summary = "Theme market catalog fixture."
default = true
permissions = []
required_binaries = []
[exports]
aliases = false
completions = []
prompt_segments = []
hooks = []
commands = []
keybindings = []
themes = ["testmarket"]
"#
        ),
    )
    .unwrap();
    fs::write(
        path.join("themes").join("testmarket.toml"),
        r#"[prompt_user]
fg = "light cyan"
bold = true
[prompt_symbol]
fg = "light magenta"
bold = true
"#,
    )
    .unwrap();
    write_test_bundle_index(
        path,
        version,
        "themes",
        "builtin",
        "ux",
        "Theme market catalog fixture.",
        true,
        &[],
        &[],
    );
}
fn write_external_theme_test_bundle(path: &Path, version: &str) {
    fs::create_dir_all(path.join("packs").join("themes")).unwrap();
    fs::create_dir_all(path.join("themes")).unwrap();
    fs::write(
        path.join("bundle.toml"),
        format!(
            r#"name = "community-tools"
version = {version:?}
api = "winuxsh:plugin-bundle@0.1.0"
min_winuxsh = "0.8.3"
[packs]
default = ["themes"]
available = ["themes"]
[layout]
packs_dir = "packs"
themes_dir = "themes"
"#
        ),
    )
    .unwrap();
    fs::write(
        path.join("packs").join("themes").join("plugin.toml"),
        format!(
            r#"name = "themes"
bundle = "community-tools"
version = {version:?}
kind = "builtin"
api = "winuxsh:plugin@0.1.0"
category = "ux"
summary = "External theme market catalog fixture."
default = true
permissions = []
required_binaries = []
[exports]
aliases = false
completions = []
prompt_segments = []
hooks = []
commands = []
keybindings = []
themes = ["testmarket"]
"#
        ),
    )
    .unwrap();
    fs::write(
        path.join("themes").join("testmarket.toml"),
        r#"[prompt_user]
fg = "light cyan"
bold = true
[prompt_symbol]
fg = "light magenta"
bold = true
"#,
    )
    .unwrap();
}
fn write_keybindings_test_bundle(path: &Path, version: &str) {
    fs::create_dir_all(path.join("packs").join("keybindings")).unwrap();
    fs::create_dir_all(path.join("keybindings")).unwrap();
    fs::write(
        path.join("bundle.toml"),
        format!(
            r#"name = "oh-my-winuxsh"
version = {version:?}
api = "winuxsh:plugin-bundle@0.1.0"
min_winuxsh = "0.8.3"
[packs]
default = ["keybindings"]
available = ["keybindings"]
[layout]
packs_dir = "packs"
keybindings_dir = "keybindings"
"#
        ),
    )
    .unwrap();
    fs::write(
        path.join("packs").join("keybindings").join("plugin.toml"),
        format!(
            r#"name = "keybindings"
bundle = "oh-my-winuxsh"
version = {version:?}
kind = "builtin"
api = "winuxsh:plugin@0.1.0"
category = "ux"
summary = "Bundle-owned keybinding metadata fixture."
default = true
permissions = []
required_binaries = []
[exports]
aliases = false
completions = []
prompt_segments = []
hooks = []
commands = []
keybindings = ["common", "emacs", "vi"]
"#
        ),
    )
    .unwrap();
    fs::write(
        path.join("keybindings").join("common.toml"),
        r#"name = "common"
summary = "Bundle common keybindings v1"
keymap = "all"
[[bindings]]
key = "Tab"
action = "complete-word"
[[bindings]]
key = "Ctrl+R"
action = "history-incremental-search-backward"
"#,
    )
    .unwrap();
    fs::write(
        path.join("keybindings").join("emacs.toml"),
        r#"name = "emacs"
summary = "Bundle emacs keybindings v1"
keymap = "emacs"
[[bindings]]
key = "Ctrl+A"
action = "beginning-of-line"
"#,
    )
    .unwrap();
    fs::write(
        path.join("keybindings").join("vi.toml"),
        r#"name = "vi"
summary = "Bundle vi keybindings v1"
keymap = "vi"
[[bindings]]
key = "Esc"
action = "vi-normal-mode"
"#,
    )
    .unwrap();
    write_test_bundle_index(
        path,
        version,
        "keybindings",
        "builtin",
        "ux",
        "Bundle-owned keybinding metadata fixture.",
        true,
        &[],
        &[],
    );
}
fn write_process_test_bundle(path: &Path, version: &str) {
    write_process_test_bundle_with_timeout(path, version, 1000);
}
fn write_source_test_bundle(path: &Path, version: &str, source_file: &str) {
    fs::create_dir_all(path.join("packs").join("source-test")).unwrap();
    fs::write(
        path.join("bundle.toml"),
        format!(
            r#"name = "oh-my-winuxsh"
version = {version:?}
api = "winuxsh:plugin-bundle@0.1.0"
min_winuxsh = "0.8.3"
[packs]
default = []
available = ["source-test"]
[layout]
packs_dir = "packs"
"#
        ),
    )
    .unwrap();
    fs::write(
        path.join("packs").join("source-test").join("plugin.toml"),
        format!(
            r#"name = "source-test"
bundle = "oh-my-winuxsh"
version = {version:?}
kind = "source"
api = "winuxsh:plugin@0.1.0"
category = "workflow"
summary = "Source plugin startup fixture."
default = false
permissions = ["shell:source"]
required_binaries = []
[exports]
aliases = true
completions = []
prompt_segments = []
hooks = ["startup"]
commands = []
keybindings = []
[source]
entry = "packs/source-test/{source_file}"
"#
        ),
    )
    .unwrap();
    fs::write(
        path.join("packs").join("source-test").join(source_file),
        "alias source_test='echo source plugin'\n",
    )
    .unwrap();
    write_test_bundle_index(
        path,
        version,
        "source-test",
        "source",
        "workflow",
        "Source plugin startup fixture.",
        false,
        &["shell:source"],
        &[],
    );
}
fn write_external_process_test_bundle(path: &Path, version: &str) {
    write_external_process_test_bundle_with_pack_bundle(path, version, "community-tools");
}
fn write_external_process_test_bundle_with_pack_bundle(
    path: &Path,
    version: &str,
    pack_bundle: &str,
) {
    fs::create_dir_all(path.join("packs").join("process-echo")).unwrap();
    fs::write(
        path.join("bundle.toml"),
        format!(
            r#"name = "community-tools"
version = {version:?}
api = "winuxsh:plugin-bundle@0.1.0"
min_winuxsh = "0.8.3"
[packs]
default = []
available = ["process-echo"]
[layout]
packs_dir = "packs"
"#
        ),
    )
    .unwrap();
    fs::write(
        path.join("packs").join("process-echo").join("plugin.toml"),
        format!(
            r#"name = "process-echo"
bundle = {pack_bundle:?}
version = {version:?}
kind = "process"
api = "winuxsh:plugin@0.1.0"
category = "workflow"
summary = "External process plugin trust fixture."
default = false
permissions = ["cwd:read", "process:run:winuxsh-process-echo"]
required_binaries = ["winuxsh-process-echo"]
[exports]
aliases = false
completions = []
prompt_segments = []
hooks = []
commands = ["process-echo"]
keybindings = []
[process]
protocol = "winuxsh:process-plugin@0.1.0"
command = "winuxsh-process-echo"
args = ["--format", "json"]
timeout_millis = 1000
"#
        ),
    )
    .unwrap();
}
fn write_process_test_bundle_with_timeout(path: &Path, version: &str, timeout_millis: u64) {
    fs::create_dir_all(path.join("packs").join("process-echo")).unwrap();
    fs::write(
        path.join("bundle.toml"),
        format!(
            r#"name = "oh-my-winuxsh"
version = {version:?}
api = "winuxsh:plugin-bundle@0.1.0"
min_winuxsh = "0.8.3"
[packs]
default = []
available = ["process-echo"]
[layout]
packs_dir = "packs"
"#
        ),
    )
    .unwrap();
    fs::write(
        path.join("packs").join("process-echo").join("plugin.toml"),
        format!(
            r#"name = "process-echo"
bundle = "oh-my-winuxsh"
version = {version:?}
kind = "process"
api = "winuxsh:plugin@0.1.0"
category = "workflow"
summary = "Process plugin host contract fixture."
default = false
permissions = ["cwd:read", "process:run:winuxsh-process-echo"]
required_binaries = ["winuxsh-process-echo"]
[exports]
aliases = false
completions = []
prompt_segments = []
hooks = []
commands = ["process-echo"]
keybindings = []
[process]
protocol = "winuxsh:process-plugin@0.1.0"
command = "winuxsh-process-echo"
args = ["--format", "json"]
timeout_millis = {timeout_millis}
"#
        ),
    )
    .unwrap();
    write_test_bundle_index(
        path,
        version,
        "process-echo",
        "process",
        "workflow",
        "Process plugin host contract fixture.",
        false,
        &["cwd:read", "process:run:winuxsh-process-echo"],
        &["winuxsh-process-echo"],
    );
}
fn write_bundle_zip_from_dir(bundle_dir: &Path, archive_path: &Path) {
    let file = fs::File::create(archive_path).unwrap();
    let mut archive = zip::ZipWriter::new(file);
    let options =
        zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    let mut files = Vec::new();
    collect_files(bundle_dir, &mut files);
    files.sort();
    for path in files {
        let relative = path
            .strip_prefix(bundle_dir)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        archive.start_file(relative, options).unwrap();
        let bytes = fs::read(&path).unwrap();
        std::io::Write::write_all(&mut archive, &bytes).unwrap();
    }
    archive.finish().unwrap();
}
fn test_file_sha256(path: &Path) -> String {
    let mut file = fs::File::open(path).unwrap();
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 8192];
    loop {
        let read = file.read(&mut buffer).unwrap();
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    format!("{:x}", hasher.finalize())
}
fn collect_files(dir: &Path, files: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            collect_files(&path, files);
        } else {
            files.push(path);
        }
    }
}
