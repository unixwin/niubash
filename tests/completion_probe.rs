//! Binary-level completion probe tests.
//!
//! These exercise `Shell::new()` plus the REPL completer without needing to
//! drive reedline through an interactive terminal.

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
fn empty_command_line_suggests_core_commands() {
    let env = ProbeEnv::new("winuxsh-completion-empty");
    let suggestions = run_probe("", &env, &[]);

    assert_contains(&suggestions, "ls");
    assert_contains(&suggestions, "grep");
}

#[test]
fn partial_command_word_suggests_command() {
    let env = ProbeEnv::new("winuxsh-completion-partial");
    let suggestions = run_probe("gre", &env, &[]);

    assert_contains(&suggestions, "grep");
}

#[test]
fn substring_completion_config_suggests_middle_command_match() {
    let env = ProbeEnv::new("winuxsh-completion-substring");
    env.write_config(
        r#"
[completions]
matching = "substring"
"#,
    );

    let suggestions = run_probe("ep", &env, &[]);

    assert_contains(&suggestions, "grep");
}

#[test]
fn command_completion_result_cap_limits_blank_tab() {
    let env = ProbeEnv::new("winuxsh-completion-result-cap");
    env.write_config(
        r#"
[completions]
max_command_results = 1
"#,
    );

    let suggestions = run_probe("", &env, &[]);

    assert_eq!(suggestions.len(), 1, "got {suggestions:?}");
}

#[test]
fn path_command_is_suggested_by_prefix() {
    if !cfg!(windows) {
        return;
    }

    let env = ProbeEnv::new("winuxsh-completion-path");
    let bin = env.root.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::write(bin.join("probecli.cmd"), "@echo off\r\necho probe\r\n").unwrap();

    let old_path = std::env::var("PATH").unwrap_or_default();
    let path = format!("{};{}", native_path(&bin), old_path);
    let suggestions = run_probe(
        "pro",
        &env,
        &[
            ("PATH", path),
            ("PATHEXT", ".COM;.EXE;.BAT;.CMD".to_string()),
        ],
    );

    assert_contains(&suggestions, "probecli");
}

#[test]
fn blank_argument_position_suggests_paths() {
    let env = ProbeEnv::new("winuxsh-completion-path-argument");
    std::fs::create_dir_all(env.start.join("adir")).unwrap();
    std::fs::write(env.start.join("alpha.txt"), "alpha").unwrap();
    std::fs::write(env.start.join(".hidden"), "hidden").unwrap();

    let suggestions = run_probe("ls ", &env, &[]);

    assert_contains(&suggestions, "adir/");
    assert_contains(&suggestions, "alpha.txt");
    assert_not_contains(&suggestions, ".hidden");
    assert_before(&suggestions, "adir/", "alpha.txt");
}

#[test]
fn dot_prefix_suggests_hidden_paths() {
    let env = ProbeEnv::new("winuxsh-completion-hidden-prefix");
    std::fs::write(env.start.join(".hidden"), "hidden").unwrap();

    let suggestions = run_probe("ls .", &env, &[]);

    assert_contains(&suggestions, ".hidden");
}

#[test]
fn cd_blank_argument_position_suggests_directories_only() {
    let env = ProbeEnv::new("winuxsh-completion-cd-argument");
    std::fs::create_dir_all(env.start.join("adir")).unwrap();
    std::fs::write(env.start.join("alpha.txt"), "alpha").unwrap();

    let suggestions = run_probe("cd ", &env, &[]);

    assert_contains(&suggestions, "adir/");
    assert_not_contains(&suggestions, "alpha.txt");
}

#[test]
fn path_completion_preserves_typed_directory_prefix() {
    let env = ProbeEnv::new("winuxsh-completion-prefix");
    let parent = env.start.join("parent");
    std::fs::create_dir_all(parent.join("adir")).unwrap();
    std::fs::write(parent.join("child.txt"), "child").unwrap();

    let directory_suggestions = run_probe("ls parent/", &env, &[]);
    assert_contains(&directory_suggestions, "parent/adir/");
    assert_contains(&directory_suggestions, "parent/child.txt");

    let partial_suggestions = run_probe("ls parent/ch", &env, &[]);
    assert_contains(&partial_suggestions, "parent/child.txt");
}

#[test]
fn tilde_path_completion_lists_home_entries() {
    let env = ProbeEnv::new("winuxsh-completion-tilde");
    std::fs::create_dir_all(env.home.join("adir")).unwrap();
    std::fs::write(env.home.join("alpha.txt"), "alpha").unwrap();

    let suggestions = run_probe("ls ~/", &env, &[]);

    assert_contains(&suggestions, "~/adir/");
    assert_contains(&suggestions, "~/alpha.txt");
}

#[test]
fn completion_probe_loads_startup_rc_aliases() {
    let env = ProbeEnv::new("winuxsh-completion-rc-alias");
    env.write_rc("alias fetch='winuxfetch.exe'\n");

    let suggestions = run_probe("fet", &env, &[]);

    assert_contains(&suggestions, "fetch");
}

#[test]
fn completion_probe_loads_startup_rc_functions() {
    let env = ProbeEnv::new("winuxsh-completion-rc-function");
    env.write_rc("fetch_repo() { git fetch; }\n");

    let suggestions = run_probe("fetch_", &env, &[]);

    assert_contains(&suggestions, "fetch_repo");
}

#[test]
fn path_completion_escapes_spaces_in_candidates() {
    let env = ProbeEnv::new("winuxsh-completion-spaces");
    std::fs::create_dir_all(env.start.join("two dir")).unwrap();
    std::fs::write(env.start.join("two words.txt"), "two").unwrap();

    let suggestions = run_probe("ls tw", &env, &[]);

    assert_contains(&suggestions, "two\\ dir/");
    assert_contains(&suggestions, "two\\ words.txt");
    assert_before(&suggestions, "two\\ dir/", "two\\ words.txt");
}

#[test]
fn case_sensitive_completion_config_respects_path_case() {
    let env = ProbeEnv::new("winuxsh-completion-case-sensitive");
    env.write_config(
        r#"
[completions]
case_sensitive = true
"#,
    );
    std::fs::write(env.start.join("Alpha.txt"), "alpha").unwrap();

    let lower = run_probe("ls a", &env, &[]);
    assert_not_contains(&lower, "Alpha.txt");

    let upper = run_probe("ls A", &env, &[]);
    assert_contains(&upper, "Alpha.txt");
}

#[test]
fn path_completion_matches_escaped_spaces_in_input() {
    let env = ProbeEnv::new("winuxsh-completion-escaped-input");
    let parent = env.start.join("parent dir");
    std::fs::create_dir_all(&parent).unwrap();
    std::fs::write(env.start.join("two words.txt"), "two").unwrap();
    std::fs::write(parent.join("child.txt"), "child").unwrap();

    let file_suggestions = run_probe("ls two\\ w", &env, &[]);
    assert_contains(&file_suggestions, "two\\ words.txt");

    let nested_suggestions = run_probe("ls parent\\ dir/ch", &env, &[]);
    assert_contains(&nested_suggestions, "parent\\ dir/child.txt");
}

#[test]
fn path_completion_matches_double_quoted_input() {
    let env = ProbeEnv::new("winuxsh-completion-quoted-input");
    std::fs::write(env.start.join("two words.txt"), "two").unwrap();

    let suggestions = run_probe("ls \"two w", &env, &[]);

    assert_contains(&suggestions, "\"two words.txt\"");
}

#[test]
fn command_position_after_pipe_suggests_command() {
    let env = ProbeEnv::new("winuxsh-completion-pipe");
    let suggestions = run_probe("ls | gre", &env, &[]);

    assert_contains(&suggestions, "grep");
}

#[test]
fn blank_command_position_after_pipe_suggests_commands() {
    let env = ProbeEnv::new("winuxsh-completion-pipe-empty");
    let suggestions = run_probe("ls | ", &env, &[]);

    assert_contains(&suggestions, "grep");
    assert_contains(&suggestions, "ls");
}

#[test]
fn argument_position_does_not_suggest_commands() {
    let env = ProbeEnv::new("winuxsh-completion-arg");
    let suggestions = run_probe("echo gre", &env, &[]);

    assert_not_contains(&suggestions, "grep");
}

#[test]
fn installed_bundle_completion_definitions_override_compiled_defaults() {
    let env = ProbeEnv::new("winuxsh-completion-bundle-def");
    let bundle = env.root.join("bundle");
    write_minimal_completion_bundle(&bundle);

    let suggestions = run_probe(
        "git --",
        &env,
        &[("WINUXSH_PLUGIN_BUNDLE_PATH", native_path(&bundle))],
    );
    assert_contains(&suggestions, "--bundle-only");
    assert_not_contains(&suggestions, "--version");

    let subcommand_suggestions = run_probe(
        "git bundle-subcommand --",
        &env,
        &[("WINUXSH_PLUGIN_BUNDLE_PATH", native_path(&bundle))],
    );
    assert_contains(&subcommand_suggestions, "--bundle-subcommand-flag");
}

#[test]
fn git_completion_suggests_daily_subcommands_and_flags() {
    let env = ProbeEnv::new("winuxsh-completion-git-daily");

    let subcommands = run_probe("git ", &env, &[]);
    assert_contains(&subcommands, "add");
    assert_contains(&subcommands, "commit");
    assert_contains(&subcommands, "push");
    assert_contains(&subcommands, "pull");
    assert_contains(&subcommands, "checkout");

    let add = run_probe("git a", &env, &[]);
    assert_contains(&add, "add");
    assert_not_contains(&add, "commit");

    let commit_flags = run_probe("git commit --", &env, &[]);
    assert_contains(&commit_flags, "--message");
    assert_contains(&commit_flags, "--amend");
    assert_contains(&commit_flags, "--no-verify");

    let push_flags = run_probe("git push --force", &env, &[]);
    assert_contains(&push_flags, "--force");
    assert_contains(&push_flags, "--force-with-lease");
}

#[test]
fn disabling_git_plugin_removes_git_completion_definitions() {
    let env = ProbeEnv::new("winuxsh-completion-git-disabled");
    env.write_config(
        r#"[winuxcmd]
enabled = false

[plugins]
enabled = true
bundles = ["oh-my-winuxsh"]
load = []

[plugins.git]
enabled = false
"#,
    );

    let flags = run_probe("git commit --", &env, &[]);

    assert_not_contains(&flags, "--message");
    assert_not_contains(&flags, "--amend");
    assert_not_contains(&flags, "--no-verify");
}

fn run_probe(line: &str, env: &ProbeEnv, extra_env: &[(&str, String)]) -> Vec<String> {
    let output = run_winuxsh_probe(line, &env.start, &env.home, extra_env);
    assert_success(&output, line);
    stdout_lines(&output)
}

fn run_winuxsh_probe(line: &str, cwd: &Path, home: &Path, extra_env: &[(&str, String)]) -> Output {
    let mut command = Command::new(winuxsh_binary());
    command
        .arg("--completion-probe")
        .arg(line)
        .current_dir(cwd)
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("WINUXSH_CONFIG", home.join(".winshrc.toml"))
        .env("ZDOTDIR", home);

    for (key, value) in extra_env {
        command.env(key, value);
    }

    command
        .output()
        .unwrap_or_else(|err| panic!("spawn winuxsh: {err}"))
}

fn assert_success(output: &Output, context: &str) {
    assert!(
        output.status.success(),
        "completion probe for {context:?} failed with status {:?}\nstdout:\n{}\nstderr:\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn stdout_lines(output: &Output) -> Vec<String> {
    normalize_text(&output.stdout)
        .lines()
        .map(|line| line.trim().to_string())
        .filter(|line| !line.is_empty())
        .collect()
}

fn normalize_text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes)
        .replace("\r\n", "\n")
        .trim()
        .to_string()
}

fn assert_contains(values: &[String], expected: &str) {
    assert!(
        values.iter().any(|value| value == expected),
        "expected {expected:?}, got {values:?}"
    );
}

fn assert_not_contains(values: &[String], unexpected: &str) {
    assert!(
        !values.iter().any(|value| value == unexpected),
        "did not expect {unexpected:?}, got {values:?}"
    );
}

fn assert_before(values: &[String], earlier: &str, later: &str) {
    let earlier_index = values
        .iter()
        .position(|value| value == earlier)
        .unwrap_or_else(|| panic!("missing {earlier:?} in {values:?}"));
    let later_index = values
        .iter()
        .position(|value| value == later)
        .unwrap_or_else(|| panic!("missing {later:?} in {values:?}"));
    assert!(
        earlier_index < later_index,
        "expected {earlier:?} before {later:?}, got {values:?}"
    );
}

fn native_path(path: &Path) -> String {
    path.to_string_lossy().to_string()
}

fn write_minimal_completion_bundle(path: &Path) {
    std::fs::create_dir_all(path.join("packs").join("git")).unwrap();
    std::fs::create_dir_all(path.join("completions")).unwrap();
    std::fs::write(
        path.join("bundle.toml"),
        r#"name = "oh-my-winuxsh"
version = "9.9.9"
api = "winuxsh:plugin-bundle@0.1.0"
min_winuxsh = "0.8.3"
[packs]
default = ["git"]
available = ["git"]
[layout]
packs_dir = "packs"
completions_dir = "completions"
"#,
    )
    .unwrap();
    std::fs::write(
        path.join("packs").join("git").join("plugin.toml"),
        r#"name = "git"
bundle = "oh-my-winuxsh"
version = "9.9.9"
kind = "builtin"
api = "winuxsh:plugin@0.1.0"
category = "devtools"
summary = "Installed Git completions"
default = true
permissions = ["cwd:read", "process:run:git"]
required_binaries = ["git"]
[exports]
aliases = false
completions = ["git"]
prompt_segments = []
hooks = []
commands = []
keybindings = []
"#,
    )
    .unwrap();
    std::fs::write(
        path.join("completions").join("git.toml"),
        r#"command = "git"
description = "test bundle git"
[[flags]]
long = "--bundle-only"
description = "flag loaded from test bundle"
[[subcommands]]
name = "bundle-subcommand"
description = "subcommand loaded from test bundle"
[[subcommands.flags]]
long = "--bundle-subcommand-flag"
description = "subcommand flag loaded from test bundle"
"#,
    )
    .unwrap();
}

struct ProbeEnv {
    root: PathBuf,
    home: PathBuf,
    start: PathBuf,
}

impl ProbeEnv {
    fn new(prefix: &str) -> Self {
        let root = unique_temp_dir(prefix);
        let home = root.join("home");
        let start = root.join("start");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&start).unwrap();
        Self { root, home, start }
    }

    fn write_config(&self, content: &str) {
        std::fs::write(self.home.join(".winshrc.toml"), content).unwrap();
    }

    fn write_rc(&self, content: &str) {
        std::fs::write(self.home.join(".winshrc"), content).unwrap();
    }
}

impl Drop for ProbeEnv {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn unique_temp_dir(prefix: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!("{}-{}-{}", prefix, std::process::id(), nanos))
}
