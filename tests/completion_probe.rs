//! Binary-level completion probe tests.
//!
//! These exercise `Shell::new()` plus the REPL completer without needing to
//! drive reedline through an interactive terminal.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

fn niu_binary() -> PathBuf {
    let p = PathBuf::from(env!("CARGO_BIN_EXE_niu"));
    if p.exists() {
        return p;
    }

    let mut fallback = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    fallback.push("target");
    fallback.push("debug");
    fallback.push(if cfg!(windows) { "niu.exe" } else { "niubash" });
    fallback
}

#[test]
fn empty_command_line_suggests_core_commands() {
    let env = ProbeEnv::new("niubash-completion-empty");
    let suggestions = run_probe("", &env, &[]);

    assert_contains(&suggestions, "ls");
    assert_contains(&suggestions, "grep");
}

#[test]
fn partial_command_word_suggests_command() {
    let env = ProbeEnv::new("niubash-completion-partial");
    let suggestions = run_probe("gre", &env, &[]);

    assert_contains(&suggestions, "grep");
}

#[test]
fn path_command_is_suggested_by_prefix() {
    if !cfg!(windows) {
        return;
    }

    let env = ProbeEnv::new("niubash-completion-path");
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
    let env = ProbeEnv::new("niubash-completion-path-argument");
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
    let env = ProbeEnv::new("niubash-completion-hidden-prefix");
    std::fs::write(env.start.join(".hidden"), "hidden").unwrap();

    let suggestions = run_probe("ls .", &env, &[]);

    assert_contains(&suggestions, ".hidden");
}

#[test]
fn cd_blank_argument_position_suggests_directories_only() {
    let env = ProbeEnv::new("niubash-completion-cd-argument");
    std::fs::create_dir_all(env.start.join("adir")).unwrap();
    std::fs::write(env.start.join("alpha.txt"), "alpha").unwrap();

    let suggestions = run_probe("cd ", &env, &[]);

    assert_contains(&suggestions, "adir/");
    assert_not_contains(&suggestions, "alpha.txt");
}

#[test]
fn path_completion_preserves_typed_directory_prefix() {
    let env = ProbeEnv::new("niubash-completion-prefix");
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
    let env = ProbeEnv::new("niubash-completion-tilde");
    std::fs::create_dir_all(env.home.join("adir")).unwrap();
    std::fs::write(env.home.join("alpha.txt"), "alpha").unwrap();

    let suggestions = run_probe("ls ~/", &env, &[]);

    assert_contains(&suggestions, "~/adir/");
    assert_contains(&suggestions, "~/alpha.txt");
}

#[test]
fn completion_probe_loads_startup_rc_aliases() {
    let env = ProbeEnv::new("niubash-completion-rc-alias");
    env.write_rc("alias fetch='winuxfetch.exe'\n");

    let suggestions = run_probe("fet", &env, &[]);

    assert_contains(&suggestions, "fetch");
}

#[test]
fn completion_probe_loads_startup_rc_functions() {
    let env = ProbeEnv::new("niubash-completion-rc-function");
    env.write_rc("function deploy_site() { echo deploy; }\n");

    let suggestions = run_probe("dep", &env, &[]);

    assert_contains(&suggestions, "deploy_site");
}

#[test]
fn path_completion_escapes_spaces_in_candidates() {
    let env = ProbeEnv::new("niubash-completion-spaces");
    std::fs::create_dir_all(env.start.join("two dir")).unwrap();
    std::fs::write(env.start.join("two words.txt"), "two").unwrap();

    let suggestions = run_probe("ls tw", &env, &[]);

    assert_contains(&suggestions, "two\\ dir/");
    assert_contains(&suggestions, "two\\ words.txt");
    assert_before(&suggestions, "two\\ dir/", "two\\ words.txt");
}

#[test]
fn path_completion_matches_escaped_spaces_in_input() {
    let env = ProbeEnv::new("niubash-completion-escaped-input");
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
    let env = ProbeEnv::new("niubash-completion-quoted-input");
    std::fs::write(env.start.join("two words.txt"), "two").unwrap();

    let suggestions = run_probe("ls \"two w", &env, &[]);

    assert_contains(&suggestions, "\"two words.txt\"");
}

#[test]
fn command_position_after_pipe_suggests_command() {
    let env = ProbeEnv::new("niubash-completion-pipe");
    let suggestions = run_probe("ls | gre", &env, &[]);

    assert_contains(&suggestions, "grep");
}

#[test]
fn blank_command_position_after_pipe_suggests_commands() {
    let env = ProbeEnv::new("niubash-completion-pipe-empty");
    let suggestions = run_probe("ls | ", &env, &[]);

    assert_contains(&suggestions, "grep");
    assert_contains(&suggestions, "ls");
}

#[test]
fn argument_position_does_not_suggest_commands() {
    let env = ProbeEnv::new("niubash-completion-arg");
    let suggestions = run_probe("echo gre", &env, &[]);

    assert_not_contains(&suggestions, "grep");
}

// ── External completion providers integrate as rc data ──────────────────────
//
// The shell core carries no third-party completion provider (the embedded
// carapace source from #243 was removed before 1.4.0): providers integrate
// through the startup rc, as data, the same way a zsh user sources a
// completion hook. These probes source mock provider code from
// ~/.niubashrc and assert candidates reach the Tab surface.

/// A provider speaking the GNU static protocol: a `complete -W` compspec
/// registered from rc-sourced code serves wordlist candidates at Tab.
#[test]
fn rc_sourced_compspec_wordlist_serves_candidates() {
    let env = ProbeEnv::new("niubash-completion-rc-compspec");
    env.write_primary_rc(
        r#"# external provider hook — standard bash completion code (data layer)
complete -W 'status switch merge rebase' providertool
"#,
    );

    let suggestions = run_probe("providertool s", &env, &[]);

    assert_contains(&suggestions, "status");
    assert_contains(&suggestions, "switch");
    assert_not_contains(&suggestions, "merge");
}

/// A provider speaking the shell-function protocol: an rc-declared compdef
/// (NIU_COMPDEFS) runs the hook through the completion bridge and serves its
/// `value<TAB>description` lines — the same shape carapace's fish output
/// emits — filtered by the typed prefix.
#[test]
fn rc_sourced_provider_hook_function_serves_described_candidates() {
    let env = ProbeEnv::new("niubash-completion-rc-provider-hook");
    env.write_primary_rc(
        r#"# external provider hook — carapace-style function, compdef registration
_carapace_provider_hook() {
    NIU_COMP_RESULT="checkout	Check out a branch"$'\n'"cherry-pick	Apply a commit"$'\n'"clean	Remove untracked files"
}
NIU_COMPDEFS="providercmd:_carapace_provider_hook"
"#,
    );

    let suggestions = run_probe("providercmd ch", &env, &[]);

    assert_contains(&suggestions, "checkout");
    assert_contains(&suggestions, "cherry-pick");
    assert_not_contains(&suggestions, "clean");
}

// ── Issue #307: the probe sources NIU_ENV / BASH_ENV like `-c` ───────────────

/// An agent init file (NIU_ENV) registering `complete` compspecs must reach
/// the probe: the `-W` wordlist serves candidates, and the `-F` registration
/// lands in the engine compspec registry the probe reads.
#[test]
fn completion_probe_loads_niu_env_compspecs() {
    let env = ProbeEnv::new("niubash-completion-niu-env");
    let env_file = env.root.join("agent.env");
    std::fs::write(
        &env_file,
        r#"# agent init file: GNU completion registrations travel as rc data
complete -W 'status switch merge rebase' envtool
_envtooldyn_hook() {
    COMPREPLY=()
}
complete -F _envtooldyn_hook envtooldyn
"#,
    )
    .unwrap();

    let suggestions = run_probe("envtool s", &env, &[("NIU_ENV", path_string(&env_file))]);

    assert_contains(&suggestions, "status");
    assert_contains(&suggestions, "switch");
    assert_not_contains(&suggestions, "merge");
}

/// NIU_ENV wins over BASH_ENV — the same precedence `niu -c` applies — so
/// only the NIU_ENV compspec is registered when both are set.
#[test]
fn completion_probe_prefers_niu_env_over_bash_env() {
    let env = ProbeEnv::new("niubash-completion-niu-env-priority");
    let niu_file = env.root.join("niu.env");
    let bash_file = env.root.join("bash.env");
    std::fs::write(&niu_file, "complete -W 'alpha beta' ptool\n").unwrap();
    std::fs::write(&bash_file, "complete -W 'gamma delta' ptool\n").unwrap();

    let suggestions = run_probe(
        "ptool a",
        &env,
        &[
            ("NIU_ENV", path_string(&niu_file)),
            ("BASH_ENV", path_string(&bash_file)),
        ],
    );

    assert_contains(&suggestions, "alpha");
    assert_not_contains(&suggestions, "gamma");
}

/// With NIU_ENV unset the probe falls back to BASH_ENV (bash-compatible).
#[test]
fn completion_probe_falls_back_to_bash_env() {
    let env = ProbeEnv::new("niubash-completion-bash-env");
    let bash_file = env.root.join("bash.env");
    std::fs::write(&bash_file, "complete -W 'gamma delta' ptool\n").unwrap();

    let suggestions = run_probe(
        "ptool g",
        &env,
        &[
            ("NIU_ENV", String::new()),
            ("BASH_ENV", path_string(&bash_file)),
        ],
    );

    assert_contains(&suggestions, "gamma");
    // The wordlist is prefix-filtered by the typed word (same as GNU
    // compgen), so candidates outside the prefix stay out.
    assert_not_contains(&suggestions, "delta");
    assert_not_contains(&suggestions, "alpha");
}

// ── Issue #306: NIU_COMPDEFS directory entries declare completion files ──────

/// A NIU_COMPDEFS line pointing at an existing directory declares
/// completion-definition files: `<cmd>.toml` (embedded-applet schema) and
/// `<cmd>.bash` (statically parsed; the file body is never executed).
#[test]
fn completion_dir_declared_in_niu_compdefs_serves_candidates() {
    let env = ProbeEnv::new("niubash-completion-compdefs-dir");
    let defs = env.root.join("compdefs");
    std::fs::create_dir_all(&defs).unwrap();
    std::fs::write(
        defs.join("mytool.toml"),
        r#"command = "mytool"
description = "fixture command"

[[flags]]
long = "--fixture-flag"
description = "declared in the completion dir"
"#,
    )
    .unwrap();
    std::fs::write(
        defs.join("bashtool.bash"),
        r#"_bashtool() {
    local opts
    opts="-h -V --verbose --output"
    case "${prev}" in
        --output|-o)
            COMPREPLY=($(compgen -f "${cur}"))
            return 0
            ;;
        *)
            COMPREPLY=()
            ;;
    esac
}
complete -F _bashtool bashtool
"#,
    )
    .unwrap();
    env.write_primary_rc(&format!("NIU_COMPDEFS=\"{}\"\n", path_string(&defs)));

    let toml_suggestions = run_probe("mytool -", &env, &[]);
    assert_contains(&toml_suggestions, "--fixture-flag");

    let bash_suggestions = run_probe("bashtool -", &env, &[]);
    assert_contains(&bash_suggestions, "--verbose");
}

// ── Issue #305: built-in definitions for common external commands ────────────
//
// git, tar, curl, ssh, docker, kubectl, and npm are not WinuxCmd applets; the
// compiled common layer must serve each of them through the probe surface.

#[test]
fn builtin_git_definition_completes_subcommands_and_flags() {
    let env = ProbeEnv::new("niubash-completion-builtin-git");

    let subcommands = run_probe("git ch", &env, &[]);
    assert_contains(&subcommands, "checkout");
    assert_contains(&subcommands, "cherry-pick");
    assert_not_contains(&subcommands, "clone");

    let flags = run_probe("git commit -", &env, &[]);
    assert_contains(&flags, "--message");
    assert_contains(&flags, "--amend");
}

#[test]
fn builtin_tar_definition_completes_flags() {
    let env = ProbeEnv::new("niubash-completion-builtin-tar");

    let suggestions = run_probe("tar --", &env, &[]);
    assert_contains(&suggestions, "--gzip");
    assert_contains(&suggestions, "--extract");
    assert_contains(&suggestions, "--auto-compress");
}

#[test]
fn builtin_curl_definition_completes_flag_values() {
    let env = ProbeEnv::new("niubash-completion-builtin-curl");

    let suggestions = run_probe("curl -X ", &env, &[]);
    assert_contains(&suggestions, "POST");
    assert_contains(&suggestions, "PUT");
    assert_not_contains(&suggestions, "--silent");
}

#[test]
fn builtin_ssh_definition_completes_flags() {
    let env = ProbeEnv::new("niubash-completion-builtin-ssh");

    let suggestions = run_probe("ssh -", &env, &[]);
    assert_contains(&suggestions, "-i");
    assert_contains(&suggestions, "-p");
    assert_contains(&suggestions, "-V");
}

#[test]
fn builtin_docker_definition_completes_subcommands_and_flags() {
    let env = ProbeEnv::new("niubash-completion-builtin-docker");

    let subcommands = run_probe("docker r", &env, &[]);
    assert_contains(&subcommands, "run");
    assert_contains(&subcommands, "rmi");

    let flags = run_probe("docker run -", &env, &[]);
    assert_contains(&flags, "--name");
    assert_contains(&flags, "-d");
}

#[test]
fn builtin_kubectl_definition_completes_subcommands_and_flags() {
    let env = ProbeEnv::new("niubash-completion-builtin-kubectl");

    let subcommands = run_probe("kubectl g", &env, &[]);
    assert_contains(&subcommands, "get");

    let flags = run_probe("kubectl get -", &env, &[]);
    assert_contains(&flags, "--all-namespaces");
    assert_contains(&flags, "-o");
}

#[test]
fn builtin_npm_definition_completes_subcommands_and_flags() {
    let env = ProbeEnv::new("niubash-completion-builtin-npm");

    let subcommands = run_probe("npm i", &env, &[]);
    assert_contains(&subcommands, "install");
    assert_contains(&subcommands, "init");

    let flags = run_probe("npm install -", &env, &[]);
    assert_contains(&flags, "--save-dev");
}

fn run_probe(line: &str, env: &ProbeEnv, extra_env: &[(&str, String)]) -> Vec<String> {
    let output = run_niu_probe(line, &env.start, &env.home, extra_env);
    assert_success(&output, line);
    stdout_lines(&output)
}

fn run_niu_probe(line: &str, cwd: &Path, home: &Path, extra_env: &[(&str, String)]) -> Output {
    let mut command = Command::new(niu_binary());
    command
        .arg("--completion-probe")
        .arg(line)
        .current_dir(cwd)
        .env("HOME", home)
        .env("USERPROFILE", home)
        // Hermetic by default: a developer machine's agent init file must not
        // leak compspecs into a probe assertion. Tests that exercise the
        // env-file surface (issue #307) override these explicitly.
        .env("NIU_ENV", "")
        .env("BASH_ENV", "");

    for (key, value) in extra_env {
        command.env(key, value);
    }

    command
        .output()
        .unwrap_or_else(|err| panic!("spawn niubash: {err}"))
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

/// Forward-slash spelling of a host path: safe to embed in an rc assignment
/// or pass as NIU_ENV/BASH_ENV on both Windows and Unix — the shell's own
/// path resolution (resolve_non_interactive_env_path) handles either form.
fn path_string(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
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

    fn write_rc(&self, content: &str) {
        std::fs::write(self.home.join(".winuxshrc"), content).unwrap();
    }

    /// Write the primary startup rc (`~/.niubashrc`).
    fn write_primary_rc(&self, content: &str) {
        std::fs::write(self.home.join(".niubashrc"), content).unwrap();
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
