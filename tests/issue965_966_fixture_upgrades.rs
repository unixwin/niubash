//! niubash#965/#966 — the #953 chains exposed two fixture gaps; this
//! target pins the upgraded fixture shapes, one chain per gap:
//!
//! * #965 — bash-preexec: the fixture must install the preexec calling
//!   path itself at load time (upstream's DEBUG trap + recursion
//!   guard), so a plainly loaded plugin fires `preexec_functions`
//!   around a real command with no hand-wired trap in the rc.
//! * #966 — fzf-git.sh: the widget must produce deterministic file
//!   candidates offline (tracked files via `git ls-files`, optional
//!   substring query), printed and inserted into the edited line.
//! * #966 — complete-alias: the wrapper must resolve the alias, look
//!   up the expanded command's completion, and fill COMPREPLY —
//!   filename completion when the expanded command has none, the
//!   expanded command's own `-F` function when it has one.
//!
//! Fail-first: on the base fixtures the bash-preexec trap is never
//! installed (no preexec mark), the fzf widget prints nothing, and
//! the alias wrapper returns an empty COMPREPLY, so every chain
//! below fails there.
//!
//! This target must NOT touch `tests/issue953_plugin_after_load_chains.rs`
//! (#964, in flight): the #966 upgrades change what the #953 chains
//! observe (the fzf engine probe's exact stdout, the alias chain's
//! step-3 "no fallback" assertion), so the #953 chains need a
//! follow-up adjustment after #964 merges — listed in the lane
//! REPORT, not done here.
//!
//! One staging split: the complete-alias chains source the entry file
//! straight from the installed tree instead of going through the
//! generated loader — a path-added source is wild, and the wild
//! adapter cannot enable an extension-less entry file (niubash#968).
//! The install step itself still runs through the real `plugin add`
//! machinery; full plugin-machinery coverage for complete-alias
//! belongs to the #953 chain once #968 is fixed.

#[path = "interactive/driver.rs"]
mod driver;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use driver::{require_pty_or_skip, NiuSession, DEFAULT_TIMEOUT};

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
    let dir = std::env::temp_dir().join(format!("niu965-{tag}-{}-{nanos}", std::process::id()));
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
    sources_root: PathBuf,
}

impl Staged {
    fn install(tag: &str, fixture_kind: &str, enable: &str) -> Staged {
        let staged = Self::install_add_only(tag, fixture_kind);
        let env_refs = staged.env_refs();
        let enable_out = run_niu_with_env(&["plugin", "enable", enable], &env_refs);
        assert_success(&enable_out, "plugin enable");
        let loader = staged
            .home
            .join(".niubash")
            .join("generated")
            .join("plugins.niu");
        assert!(
            loader.is_file(),
            "enable must publish the generated loader: {}",
            loader.display()
        );
        Staged { loader, ..staged }
    }

    /// Install only (`plugin add`, no enable): for fixtures whose
    /// entry file the wild adapter cannot see. complete-alias's entry
    /// is extension-less, so `plugin enable
    /// complete-alias/complete_alias` fails on a path-added source
    /// (niubash#968 — the recipe spelling only works for
    /// catalog-driven adds); those chains source the entry file
    /// directly from the installed tree instead.
    fn install_add_only(tag: &str, fixture_kind: &str) -> Staged {
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
        Staged {
            root,
            home,
            envs,
            loader: PathBuf::new(),
            sources_root,
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

    /// Source line for an entry file straight from the installed
    /// tree (the #968 workaround: no generated loader covers it).
    fn entry_source_line(&self, rel: &str) -> String {
        format!(
            ". \"{}\"",
            path_string(&self.sources_root.join(rel))
        )
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

fn git_available() -> bool {
    Command::new("git")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn require_git_or_skip(name: &str) -> bool {
    if git_available() {
        return true;
    }
    eprintln!("skipping {name}: git not on PATH");
    false
}

/// A temp git repo with three tracked files; returns the repo dir.
/// `git ls-files` lists tracked files in sorted order.
fn make_git_repo(tag: &str) -> PathBuf {
    let repo = temp_root(tag);
    let run = |args: &[&str]| {
        let out = Command::new("git")
            .args(args)
            .current_dir(&repo)
            .output()
            .unwrap_or_else(|err| panic!("git {args:?}: {err}"));
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    run(&["-c", "init.defaultBranch=main", "init", "-q"]);
    for name in ["alpha.txt", "beta.txt", "zebra.txt"] {
        fs::write(repo.join(name), format!("{name}\n")).unwrap();
    }
    run(&["add", "."]);
    repo
}

// ---------------------------------------------------------------------
// #965 — bash-preexec: the plugin installs its own DEBUG trap.
// ---------------------------------------------------------------------

#[test]
fn bash_preexec_installs_its_debug_trap_at_load() {
    if !require_pty_or_skip("bash_preexec_installs_its_debug_trap_at_load") {
        return;
    }
    let staged = Staged::install("preexec965", "bash-preexec", "bash-preexec/bash-preexec.sh");

    // Step 0 (engine): loading the plugin must install the DEBUG trap
    // on the preexec runner — no hand-wired trap anywhere.
    let probe = staged.engine(&format!(
        "{}; printf '%s' \"$(trap -p DEBUG)\"",
        staged.loader_source_line()
    ));
    assert_success(&probe, "engine trap probe");
    assert!(
        stdout_text(&probe).contains("__bp_run_preexec"),
        "loading must install the DEBUG trap on __bp_run_preexec: {}",
        stdout_text(&probe)
    );

    // Live session: register marker hooks into the plugin's own
    // arrays. The rc carries NO trap line — the trap must come from
    // the plugin itself, installed at load time.
    let state = staged.root.join("bp965-state.txt");
    let rc_extra = format!(
        "BP965_STATE='{}'\n\
         bp965_precmd_mark() {{ printf 'precmd\\n' >> \"$BP965_STATE\"; }}\n\
         bp965_preexec_mark() {{ printf 'preexec\\n' >> \"$BP965_STATE\"; }}\n\
         precmd_functions+=(bp965_precmd_mark)\n\
         preexec_functions+=(bp965_preexec_mark)\n",
        path_string(&state)
    );
    let mut session = spawn_session(&staged, &rc_extra);
    // wait_ready already consumes the startup prompt paint (the
    // session paints P1> exactly once at startup); a second
    // expect_prompt here would wait for a prompt that never comes.
    session.wait_ready();

    // Step 1: with no command typed yet, the prompt cycle alone must
    // have fired the precmd hook — proof the PROMPT_COMMAND wrapper
    // the plugin installed runs on its own.
    let boot = poll_state(&state, |l| l.iter().any(|x| x == "precmd"), DEFAULT_TIMEOUT);
    assert!(
        boot.iter().any(|x| x == "precmd"),
        "precmd hook must fire at the prompt cycle: state={boot:?}\n{}",
        session.transcript()
    );

    // Step 2: a real command runs; its own marker lands between a
    // preexec mark (before the command body) and the next precmd
    // mark (the following prompt cycle). No trap was installed by
    // the test — the preexec mark can only come from the plugin.
    session.send_line("printf 'cmd\\n' >> \"$BP965_STATE\"");
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
// #966 — fzf-git.sh: the widget produces deterministic candidates.
// ---------------------------------------------------------------------

#[test]
fn fzf_git_files_lists_tracked_files_deterministically() {
    if !require_git_or_skip("fzf_git_files_lists_tracked_files_deterministically") {
        return;
    }
    let staged = Staged::install("fzfgit966", "fzf-git.sh", "fzf-git.sh/fzf-git.sh");
    let repo = make_git_repo("fzf966");

    // Step 0: the widget lists the repo's tracked files in
    // `git ls-files` order.
    let probe = staged.engine(&format!(
        "{}; cd \"{}\"; fzf_git_files",
        staged.loader_source_line(),
        path_string(&repo)
    ));
    assert_success(&probe, "engine widget probe");
    assert_eq!(
        stdout_text(&probe),
        "alpha.txt\nbeta.txt\nzebra.txt\n",
        "widget must list tracked files deterministically"
    );

    // Step 1: an optional query filters the list by substring.
    let probe = staged.engine(&format!(
        "{}; cd \"{}\"; fzf_git_files zeb",
        staged.loader_source_line(),
        path_string(&repo)
    ));
    assert_success(&probe, "engine widget query probe");
    assert_eq!(
        stdout_text(&probe),
        "zebra.txt\n",
        "a query must filter the candidate list"
    );

    // Step 2: when the line editor is present, the widget inserts
    // the first candidate into the line being edited.
    let probe = staged.engine(&format!(
        "{}; cd \"{}\"; READLINE_LINE='zz'; fzf_git_files >/dev/null; printf '%s' \"$READLINE_LINE\"",
        staged.loader_source_line(),
        path_string(&repo)
    ));
    assert_success(&probe, "engine widget line-insert probe");
    assert_eq!(
        stdout_text(&probe),
        "alpha.txt",
        "the widget must insert the first candidate into READLINE_LINE"
    );

    let _ = fs::remove_dir_all(&repo);
}

#[test]
fn fzf_git_widget_key_press_lists_candidates() {
    if !require_pty_or_skip("fzf_git_widget_key_press_lists_candidates") {
        return;
    }
    if !require_git_or_skip("fzf_git_widget_key_press_lists_candidates") {
        return;
    }
    let staged = Staged::install("fzfgit966pty", "fzf-git.sh", "fzf-git.sh/fzf-git.sh");
    let repo = make_git_repo("fzf966pty");

    // Live session: bind the widget to a key and make its execution
    // observable via xtrace, as in the #953 chain.
    let rc_extra = "PS4='XTRACE966 '\n\
                    set -x\n\
                    bind -x '\"\\C-gf\": fzf_git_files'\n";
    let mut session = spawn_session(&staged, rc_extra);
    // wait_ready already consumes the startup prompt paint; see the
    // #965 chain above.
    session.wait_ready();

    // Move into the repo, then press the chord: the widget must print
    // the deterministic candidate list into the transcript.
    session.send_line(&format!("cd \"{}\"", path_string(&repo)));
    session.expect_prompt();
    session.send("zz");
    session.send("\u{7}f");
    session.expect("XTRACE966 fzf_git_files");
    session.expect("zebra.txt");

    // Synchronize on the widget finishing: the prompt repaints only
    // after the bind -x command returns, so this expect cannot land
    // mid-widget (an earlier revision sent Ctrl-C the moment the list
    // printed and aborted the widget before its READLINE_LINE
    // assignment ran). The repaint carries the replaced line.
    session.expect_prompt();

    // The widget's READLINE_LINE assignment must have reached the
    // editor: submitting the line runs the first candidate, not the
    // typed "zz" — the xtrace of the submitted command proves it.
    session.send("\r");
    session.expect("XTRACE966 alpha.txt");
    session.expect_prompt();
    session.send_line("exit");
    assert_eq!(session.wait_exit(), 0, "{}", session.transcript());

    let _ = fs::remove_dir_all(&repo);
}

// ---------------------------------------------------------------------
// #966 — complete-alias: the wrapper delegates to the expansion.
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
fn complete_alias_delegates_filename_completion() {
    // Add-only staging: the entry file is extension-less, so the
    // wild adapter cannot enable it (niubash#968); the chain sources
    // the entry straight from the installed tree instead. The
    // install step itself still goes through the real machinery.
    let staged = Staged::install_add_only("compalias966", "complete-alias");
    let entry = staged.entry_source_line("complete-alias/complete_alias");

    // Step 0 (engine): sourcing the installed entry defines the wrapper.
    let probe = staged.engine(&format!(
        "{}; printf '%s' \"$(type -t __complete_alias)\"",
        entry
    ));
    assert_success(&probe, "engine wrapper probe");
    assert_eq!(stdout_text(&probe).trim_end(), "function");

    // A real command line to complete: alias `ll`, and a directory
    // whose filenames the expanded command's completion offers.
    let start = staged.root.join("start");
    fs::create_dir_all(&start).unwrap();
    fs::write(start.join("zebra953.txt"), "z\n").unwrap();
    fs::write(start.join("notes953.txt"), "n\n").unwrap();

    // Register the plugin's wrapper for the alias and complete:
    // the wrapper must delegate to the expansion (`ls -l`), whose
    // default completion is filename completion — so the candidate
    // the #953 stub chain proved absent must now be offered.
    let rc_wrapped = format!(
        "{}\nalias ll='ls -l'\ncomplete -F __complete_alias ll\n",
        entry
    );
    fs::write(staged.home.join(".niubashrc"), &rc_wrapped).unwrap();
    let wrapped = run_completion_probe("ll zeb", &staged, &start);
    assert!(
        wrapped.iter().any(|c| c == "zebra953.txt"),
        "the wrapper must offer the expanded command's filename candidate: {wrapped:?}"
    );
    assert!(
        !wrapped.iter().any(|c| c == "notes953.txt"),
        "only the matching prefix may be offered: {wrapped:?}"
    );

    let _ = fs::remove_dir_all(&staged.root);
}

#[test]
fn complete_alias_delegates_to_expanded_command_compspec() {
    // Add-only staging, as above (niubash#968).
    let staged = Staged::install_add_only("compalias966b", "complete-alias");
    let entry = staged.entry_source_line("complete-alias/complete_alias");

    // The expanded command has its own -F compspec returning a
    // distinctive candidate; the wrapper must call that function
    // with the rewritten word list instead of falling back to
    // filenames.
    let start = staged.root.join("start");
    fs::create_dir_all(&start).unwrap();
    fs::write(start.join("zebra953.txt"), "z\n").unwrap();

    let rc_wrapped = format!(
        "{}\n_mycomp() {{ COMPREPLY=(delegated953); }}\ncomplete -F _mycomp realcmd\nalias al='realcmd'\ncomplete -F __complete_alias al\n",
        entry
    );
    fs::write(staged.home.join(".niubashrc"), &rc_wrapped).unwrap();
    let wrapped = run_completion_probe("al x", &staged, &start);
    assert!(
        wrapped.iter().any(|c| c == "delegated953"),
        "the wrapper must delegate to the expanded command's -F function: {wrapped:?}"
    );
    assert!(
        !wrapped.iter().any(|c| c == "zebra953.txt"),
        "the delegated result must replace the filename fallback: {wrapped:?}"
    );

    let _ = fs::remove_dir_all(&staged.root);
}
