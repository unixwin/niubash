//! niubash#953 — plugin-source fixture zoo: after-load call chains.
//!
//! The fixture zoo (`tests/fixtures/sources/`) proves the plugin
//! *system* (add / enable / load shapes) in `plugin_anyplug.rs` and
//! friends, but the plugins' own functions were never called after
//! load. Per the eco-asset exercise-depth rule (every asset must
//! actually be *called* end-to-end, not merely loaded), this target
//! drives the three chains the census named, one test per plugin,
//! each with stepwise intermediate assertions:
//!
//! * bash-preexec — in a live interactive session over the installed
//!   plugin, the precmd hook fires on its own around a real command
//!   (the fixture's `PROMPT_COMMAND` wrapper runs it at the prompt
//!   cycle), and the preexec hook fires before the command body via
//!   the runner upstream wires to the DEBUG trap. The ordered state
//!   file proves precmd < preexec < command < precmd.
//! * fzf-git.sh — the widget function the plugin installs is invoked
//!   through a real `bind -x` key in a live session (upstream binds
//!   the `_fzf_git_*` widgets through fzf's key-bindings channel;
//!   the fixture's widget is `fzf_git_files`). xtrace (PS4-tagged)
//!   makes the widget's execution observable in the transcript.
//! * complete-alias — the plugin's completion wrapper is registered
//!   for a real alias and a real completion attempt (the host's
//!   `--completion-probe` face) routes through it: with the wrapper
//!   registered, its result is authoritative and the filename
//!   fallback the same line gets without it is gone.
//!
//! Fixture shapes are the modeled ones in `tests/fixtures/sources/`
//! (see each fixture's README); chains exercise exactly the call
//! paths those shapes provide.

#[path = "interactive/driver.rs"]
mod driver;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use driver::{require_pty_or_skip, NiuSession, CTRL_C, DEFAULT_TIMEOUT};

fn niu_binary() -> PathBuf {
    driver::niu_binary()
}

fn run_niu_with_env(args: &[&str], envs: &[(&str, String)]) -> Output {
    let mut command = Command::new(niu_binary());
    command.args(args);
    for (key, value) in envs {
        command.env(key, value);
    }
    command
        .output()
        .unwrap_or_else(|err| panic!("failed to run niubash {args:?}: {err}"))
}

fn assert_success(output: &Output, context: &str) {
    assert!(
        output.status.success(),
        "{context} failed with {}:\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn stdout_text(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn fixture(kind: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("sources")
        .join(kind)
}

fn temp_root(tag: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("niu953-{tag}-{}-{nanos}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// Forward-slash spelling of a host path, safe to embed in rc text on
/// both Windows and Unix (the shell's own path resolution handles it).
fn path_string(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

/// A plugin staged through the real machinery: `plugin add` the
/// fixture tree, `plugin enable` one asset; the generated loader is
/// the activation plane the session / probe then consumes.
struct Staged {
    root: PathBuf,
    home: PathBuf,
    envs: Vec<(String, String)>,
    loader: PathBuf,
}

impl Staged {
    fn install(tag: &str, fixture_kind: &str, enable: &str) -> Staged {
        let root = temp_root(tag);
        let home = root.join("home");
        let sources_root = root.join("sources");
        fs::create_dir_all(&home).unwrap();
        let envs: Vec<(String, String)> = vec![
            ("HOME".into(), path_string(&home)),
            ("USERPROFILE".into(), path_string(&home)),
            ("NIU_PLUGIN_SOURCES_ROOT".into(), path_string(&sources_root)),
            (
                "NIU_PLUGIN_SPEC".into(),
                path_string(&home.join(".niubash").join("plugins.json")),
            ),
        ];
        let env_refs: Vec<(&str, String)> =
            envs.iter().map(|(k, v)| (k.as_str(), v.clone())).collect();
        let origin = fixture(fixture_kind);
        let add = run_niu_with_env(&["plugin", "add", &origin.to_string_lossy()], &env_refs);
        assert_success(&add, "plugin add");
        assert!(
            stdout_text(&add).contains("Installed source '"),
            "{}",
            stdout_text(&add)
        );
        let enable_out = run_niu_with_env(&["plugin", "enable", enable], &env_refs);
        assert_success(&enable_out, "plugin enable");
        let loader = home.join(".niubash").join("generated").join("plugins.niu");
        assert!(
            loader.is_file(),
            "enable must publish the generated loader: {}",
            loader.display()
        );
        Staged {
            root,
            home,
            envs,
            loader,
        }
    }

    fn env_refs(&self) -> Vec<(&str, String)> {
        self.envs
            .iter()
            .map(|(k, v)| (k.as_str(), v.clone()))
            .collect()
    }

    /// Engine leg: run `niu -c` over the staged home (the loader is
    /// sourced by the script itself, the plugin_anyplug pattern).
    fn engine(&self, script: &str) -> Output {
        run_niu_with_env(&["-c", script], &self.env_refs())
    }

    fn loader_source_line(&self) -> String {
        format!(". \"{}\"", path_string(&self.loader))
    }
}

/// PATH for PTY sessions: real coreutils on unix (the driver's default
/// is a System32 placeholder that leaves unix sessions builtin-only);
/// Windows keeps the driver default (niu's applet injection serves).
fn gate_path_dirs() -> Vec<PathBuf> {
    if cfg!(windows) {
        Vec::new()
    } else {
        vec![PathBuf::from("/usr/bin"), PathBuf::from("/bin")]
    }
}

fn spawn_session(staged: &Staged, rc_extra: &str) -> NiuSession {
    let rc = format!("{}{}", driver::default_rc(), rc_extra);
    let extra_env: Vec<(String, String)> = staged.envs.clone();
    NiuSession::spawn_shared_home(
        &staged.root,
        &rc,
        &extra_env,
        &gate_path_dirs(),
        (100, 30),
        DEFAULT_TIMEOUT,
    )
}

/// Poll a marker file until `pred` accepts its lines or the budget
/// runs out; returns the final lines either way for assertion output.
fn poll_state(path: &Path, pred: impl Fn(&[String]) -> bool, budget: Duration) -> Vec<String> {
    let deadline = Instant::now() + budget;
    loop {
        let lines: Vec<String> = fs::read_to_string(path)
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect();
        if pred(&lines) || Instant::now() >= deadline {
            return lines;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// True when `needle` appears in `lines` as an ordered subsequence.
fn is_subsequence(lines: &[String], needle: &[&str]) -> bool {
    let mut rest = lines.iter();
    needle.iter().all(|want| rest.any(|line| line == want))
}

// ---------------------------------------------------------------------
// bash-preexec: hooks fire around a real command in a live session.
// ---------------------------------------------------------------------

#[test]
fn bash_preexec_hooks_fire_around_a_real_command() {
    if !require_pty_or_skip("bash_preexec_hooks_fire_around_a_real_command") {
        return;
    }
    let staged = Staged::install("preexec", "bash-preexec", "bash-preexec/bash-preexec.sh");

    // Step 0 (engine): post-install, the loader defines the hook
    // machinery and wraps PROMPT_COMMAND exactly as a manual source.
    let probe = staged.engine(&format!(
        "{}; printf '%s|%s' \"$(type -t __bp_run_precmd)\" \"${{PROMPT_COMMAND-}}\"",
        staged.loader_source_line()
    ));
    assert_success(&probe, "engine loader probe");
    let probe_out = stdout_text(&probe);
    assert!(
        probe_out.starts_with("function|") && probe_out.contains("__bp_run_precmd"),
        "loader must define the runner and wrap PROMPT_COMMAND: {probe_out}"
    );

    // Live session: register marker hooks into the plugin's own
    // arrays; preexec is wired through the runner the way upstream
    // wires it (DEBUG trap), precmd needs no wiring — the fixture's
    // PROMPT_COMMAND wrapper is the wiring.
    let state = staged.root.join("bp-state.txt");
    let rc_extra = format!(
        "BP953_STATE='{}'\n\
         bp953_precmd_mark() {{ printf 'precmd\\n' >> \"$BP953_STATE\"; }}\n\
         bp953_preexec_mark() {{ printf 'preexec\\n' >> \"$BP953_STATE\"; }}\n\
         precmd_functions+=(bp953_precmd_mark)\n\
         preexec_functions+=(bp953_preexec_mark)\n\
         trap '__bp_run_preexec' DEBUG\n",
        path_string(&state)
    );
    let mut session = spawn_session(&staged, &rc_extra);
    session.wait_ready();
    session.expect_prompt();

    // Step 1: with no command typed yet, the prompt cycle alone must
    // have fired the precmd hook — proof the PROMPT_COMMAND wrapper
    // the plugin installed runs on its own, not by manual invocation.
    let boot = poll_state(&state, |l| l.iter().any(|x| x == "precmd"), DEFAULT_TIMEOUT);
    assert!(
        boot.iter().any(|x| x == "precmd"),
        "precmd hook must fire at the prompt cycle: state={boot:?}\n{}",
        session.transcript()
    );

    // Step 2: a real command runs; its own marker lands between a
    // preexec mark (before the command body) and the next precmd
    // mark (the following prompt cycle).
    session.send_line("printf 'cmd\\n' >> \"$BP953_STATE\"");
    let after = poll_state(
        &state,
        |l| is_subsequence(l, &["preexec", "cmd", "precmd"]),
        DEFAULT_TIMEOUT,
    );
    assert!(
        is_subsequence(&after, &["precmd", "preexec", "cmd", "precmd"]),
        "hooks must fire around the command in order \
         (precmd, preexec, cmd, precmd): state={after:?}\n{}",
        session.transcript()
    );

    session.send_line("exit");
    assert_eq!(session.wait_exit(), 0, "{}", session.transcript());
}

// ---------------------------------------------------------------------
// fzf-git.sh: the widget is invoked through a real bind -x key.
// ---------------------------------------------------------------------

#[test]
fn fzf_git_widget_invoked_through_bind_x() {
    if !require_pty_or_skip("fzf_git_widget_invoked_through_bind_x") {
        return;
    }
    let staged = Staged::install("fzfgit", "fzf-git.sh", "fzf-git.sh/fzf-git.sh");

    // Step 0 (engine): post-install the widget function is defined
    // and runs (the fixture's widget body is upstream's entry point
    // shape; here it must at least execute and return success).
    let probe = staged.engine(&format!(
        "{}; printf '%s|%s' \"$fzf_git_fixture_imported\" \"$(type -t fzf_git_files)\"; \
         fzf_git_files; printf '|%s' \"$?\"",
        staged.loader_source_line()
    ));
    assert_success(&probe, "engine widget probe");
    assert_eq!(
        stdout_text(&probe).trim_end(),
        "1|function|0",
        "widget must be defined post-install and execute"
    );

    // Live session: bind the widget to a key (bind -x is the channel
    // the issue names) and make its execution observable via xtrace
    // under a distinctive PS4 tag.
    let rc_extra = "PS4='XTRACE953 '\n\
                    set -x\n\
                    bind -x '\"\\C-gf\": fzf_git_files'\n";
    let mut session = spawn_session(&staged, rc_extra);
    session.wait_ready();
    session.expect_prompt();

    // Negative step: the widget has not run yet — no trace line may
    // name it as an executed command (the bind line's own trace says
    // `bind -x ...`, never the bare widget call).
    assert!(
        !session.transcript().contains("XTRACE953 fzf_git_files"),
        "widget must not run before its key is pressed\n{}",
        session.transcript()
    );

    // Press the chord (Ctrl-G, f) on a partially typed line: the
    // widget executes in the live editor.
    session.send("zz");
    session.send("\u{7}f");
    session.expect("XTRACE953 fzf_git_files");

    // The session survives the widget: the line can be discarded and
    // the shell still answers.
    session.send(CTRL_C);
    session.expect_prompt();
    session.send_line("exit");
    assert_eq!(session.wait_exit(), 0, "{}", session.transcript());
}

// ---------------------------------------------------------------------
// complete-alias: a real completion attempt routes through the
// plugin's wrapper function.
// ---------------------------------------------------------------------

fn run_completion_probe(line: &str, staged: &Staged, start: &Path) -> Vec<String> {
    let mut command = Command::new(niu_binary());
    command
        .arg("--completion-probe")
        .arg(line)
        .current_dir(start)
        .env("HOME", &staged.home)
        .env("USERPROFILE", &staged.home)
        .env("NIU_ENV", "")
        .env("BASH_ENV", "");
    for (key, value) in &staged.envs {
        command.env(key, value);
    }
    let output = command
        .output()
        .unwrap_or_else(|err| panic!("spawn niubash probe: {err}"));
    assert_success(&output, line);
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(|line| line.trim().to_string())
        .filter(|line| !line.is_empty())
        .collect()
}

#[test]
fn complete_alias_wrapper_completes_a_real_command_line() {
    let staged = Staged::install(
        "compalias",
        "complete-alias",
        "complete-alias/complete_alias",
    );

    // Step 0 (engine): post-install, the loader defines the wrapper.
    let probe = staged.engine(&format!(
        "{}; printf '%s' \"$(type -t __complete_alias)\"",
        staged.loader_source_line()
    ));
    assert_success(&probe, "engine wrapper probe");
    assert_eq!(stdout_text(&probe).trim_end(), "function");

    // A real command line to complete: alias `ll`, and a directory
    // whose filenames the fallback would offer for its argument.
    let start = staged.root.join("start");
    fs::create_dir_all(&start).unwrap();
    fs::write(start.join("zebra953.txt"), "z\n").unwrap();
    fs::write(start.join("notes953.txt"), "n\n").unwrap();

    // Step 1 (baseline): rc loads the plugin and defines the alias
    // but registers no wrapper — completing `ll zeb` falls back to
    // filename completion for the alias's argument position.
    let rc_baseline = format!("{}\nalias ll='ls -l'\n", staged.loader_source_line());
    fs::write(staged.home.join(".niubashrc"), &rc_baseline).unwrap();
    let baseline = run_completion_probe("ll zeb", &staged, &start);
    assert!(
        baseline.iter().any(|c| c == "zebra953.txt"),
        "baseline must offer the filename candidate: {baseline:?}"
    );

    // Step 2: register the plugin's wrapper for the alias (the
    // complete-alias usage model: `complete -F <wrapper> <alias>`)
    // and confirm the spec is recorded.
    let spec = staged.engine(&format!(
        "{}; alias ll='ls -l'; complete -F __complete_alias ll; complete -p ll",
        staged.loader_source_line()
    ));
    assert_success(&spec, "spec registration");
    assert!(
        stdout_text(&spec).contains("__complete_alias"),
        "the compspec must name the plugin wrapper: {}",
        stdout_text(&spec)
    );

    // Step 3: the same completion attempt now routes through the
    // wrapper — its result is authoritative, so the filename
    // fallback from step 1 is gone.
    let rc_wrapped = format!(
        "{}\nalias ll='ls -l'\ncomplete -F __complete_alias ll\n",
        staged.loader_source_line()
    );
    fs::write(staged.home.join(".niubashrc"), &rc_wrapped).unwrap();
    let wrapped = run_completion_probe("ll zeb", &staged, &start);
    assert!(
        !wrapped.iter().any(|c| c == "zebra953.txt"),
        "the wrapper's result must replace the filename fallback: {wrapped:?}"
    );
    assert!(
        !wrapped.iter().any(|c| c == "notes953.txt"),
        "no fallback candidate may leak through the wrapper: {wrapped:?}"
    );

    let _ = fs::remove_dir_all(&staged.root);
}
