//! Binary-level integration for the first-class external plugin ecosystem
//! (owner rulings 2026-10-02): `niu plugin add/list/enable/disable` over
//! the *real* manager assets — oh-my-bash rc arrays, bash-it enabled/
//! entries, bash-completion whole-source activation — plus the graded
//! trust protocol (hash lock -> local signature) and the lockfile verbs
//! (restore/sync/clean).
use std::fs;
use std::path::PathBuf;
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

fn run_niu_with_env(args: &[&str], envs: &[(&str, String)]) -> Output {
    let mut command = Command::new(niu_binary());
    command.args(args);
    for (key, value) in envs {
        command.env(key, value);
    }
    command
        .output()
        .unwrap_or_else(|err| panic!("failed to run niu {args:?}: {err}"))
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

fn temp_dir(name: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("niubash-{name}-{}-{nanos}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn fixture(kind: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("sources")
        .join(kind)
}

/// HOME + USERPROFILE + NIU_PLUGIN_SOURCES_ROOT pointed at one sandbox so
/// enable/disable write their rc blocks into the sandbox, never the real
/// user home.
struct Sandbox {
    home: PathBuf,
    sources_root: PathBuf,
}

impl Sandbox {
    fn new(label: &str) -> (Self, Vec<(&'static str, String)>) {
        let temp = temp_dir(label);
        let home = temp.join("home");
        let sources_root = temp.join("sources");
        fs::create_dir_all(&home).unwrap();
        let envs = vec![
            ("HOME", home.to_string_lossy().into_owned()),
            ("USERPROFILE", home.to_string_lossy().into_owned()),
            (
                "NIU_PLUGIN_SOURCES_ROOT",
                sources_root.to_string_lossy().into_owned(),
            ),
        ];
        (Sandbox { home, sources_root }, envs)
    }
}

fn rc_of(sandbox: &Sandbox) -> String {
    fs::read_to_string(sandbox.home.join(".niubashrc")).unwrap_or_default()
}

/// Full OMB first-class lifecycle: catalog add (`--path` keeps it offline)
/// -> trust -> enable plugin/alias/completion/theme through the manager's
/// own rc arrays -> engine load of the enabled plugin -> undo.
#[test]
fn oh_my_bash_assets_enable_disable_through_rc_arrays() {
    let (sandbox, envs) = Sandbox::new("omb-assets");

    // Install offline: catalog id + local origin (same command shape as
    // `niu plugin add oh-my-bash`, whose origin comes from the catalog).
    let add = run_niu_with_env(
        &[
            "plugin",
            "add",
            "oh-my-bash",
            "--path",
            &fixture("oh-my-bash").to_string_lossy(),
        ],
        &envs,
    );
    assert_success(&add, "plugin add oh-my-bash --path");
    let add_out = stdout_text(&add);
    assert!(
        add_out.contains("Installed source 'oh-my-bash'"),
        "{add_out}"
    );
    assert!(add_out.contains("niu plugin trust oh-my-bash"), "{add_out}");

    // Untrusted: list hides assets and enable refuses.
    let list_untrusted = run_niu_with_env(&["plugin", "list"], &envs);
    assert_success(&list_untrusted, "plugin list untrusted");
    assert!(
        stdout_text(&list_untrusted).contains("assets hidden until trust"),
        "{}",
        stdout_text(&list_untrusted)
    );
    let gate = run_niu_with_env(&["plugin", "enable", "oh-my-bash"], &envs);
    assert!(!gate.status.success(), "untrusted enable must fail");
    assert!(
        String::from_utf8_lossy(&gate.stderr).contains("untrusted"),
        "{}",
        String::from_utf8_lossy(&gate.stderr)
    );

    let trust = run_niu_with_env(&["plugin", "trust", "oh-my-bash"], &envs);
    assert_success(&trust, "plugin trust");

    // Enable: source, plugin, alias, completion, theme.
    for target in ["oh-my-bash", "git", "cargo", "gradle", "agnoster"] {
        let enable = run_niu_with_env(&["plugin", "enable", target], &envs);
        assert_success(&enable, &format!("enable {target}"));
    }
    let rc = rc_of(&sandbox);
    assert!(rc.contains("OSH_THEME='agnoster'"), "{rc}");
    assert!(rc.contains("plugins=('git')"), "{rc}");
    assert!(rc.contains("aliases=('cargo')"), "{rc}");
    assert!(rc.contains("completions=('gradle')"), "{rc}");
    assert!(rc.contains("if [ -f "), "{rc}");

    // The overview marks the enabled assets and the activation.
    let list = run_niu_with_env(&["plugin", "list"], &envs);
    assert_success(&list, "plugin list ready");
    let list_out = stdout_text(&list);
    assert!(list_out.contains("*agnoster"), "{list_out}");
    assert!(list_out.contains("*git"), "{list_out}");
    assert!(!list_out.contains("not wired into"), "{list_out}");

    // Engine proof: the enabled plugin script itself sources cleanly and
    // defines its alias under the niubash engine (the guarded oh-my-bash
    // loader is a no-op in -c mode by its own interactive guard).
    let plugin_script = sandbox
        .sources_root
        .join("oh-my-bash/plugins/git/git.plugin.sh");
    let script = format!(". {}; alias gsfixture", plugin_script.to_string_lossy());
    let engine = run_niu_with_env(&["-c", &script], &envs);
    assert_success(&engine, "engine plugin load");
    assert_eq!(
        stdout_text(&engine).trim_end(),
        "alias gsfixture='git status --short --branch'"
    );

    // Undo path: each disable reverses one line; source disable removes
    // the block but keeps the tree.
    let disable = run_niu_with_env(&["plugin", "disable", "agnoster"], &envs);
    assert_success(&disable, "disable theme");
    assert!(
        !rc_of(&sandbox).contains("OSH_THEME="),
        "{}",
        rc_of(&sandbox)
    );
    let disable_source = run_niu_with_env(&["plugin", "disable", "oh-my-bash"], &envs);
    assert_success(&disable_source, "disable source");
    assert!(!rc_of(&sandbox).contains(">>> niu source oh-my-bash"));
    assert!(
        sandbox
            .sources_root
            .join("oh-my-bash/oh-my-bash.sh")
            .is_file(),
        "tree survives source disable"
    );

    let _ = fs::remove_dir_all(&sandbox.home.parent().unwrap());
}

/// bash-it assets activate through the manager's own `enabled/` entries;
/// the engine sources bash_it.sh and picks them up, theme included.
#[test]
fn bash_it_assets_activate_through_enabled_dir() {
    let (sandbox, envs) = Sandbox::new("bash-it-assets");
    let add = run_niu_with_env(
        &[
            "plugin",
            "add",
            "bash-it",
            "--path",
            &fixture("bash-it").to_string_lossy(),
        ],
        &envs,
    );
    assert_success(&add, "plugin add bash-it");
    assert!(stdout_text(&add).contains("Installed source 'bash-it'"));
    assert_success(
        &run_niu_with_env(&["plugin", "trust", "bash-it"], &envs),
        "trust bash-it",
    );

    // Plugin with a declared priority + theme.
    assert_success(
        &run_niu_with_env(&["plugin", "enable", "base"], &envs),
        "enable base",
    );
    assert_success(
        &run_niu_with_env(&["plugin", "enable", "demox"], &envs),
        "enable demox",
    );
    let entry = sandbox
        .sources_root
        .join("bash-it/enabled/350---base.plugin.bash");
    assert!(entry.is_file(), "enabled/ entry carries the priority");

    // Engine end-to-end: the rc block sources bash_it.sh (libs + enabled/
    // entries + theme), so _base_fn exists and PS1 took the theme.
    let rc = sandbox.home.join(".niubashrc");
    let load = format!(". {}; _base_fn; printf '%s' \"$PS1\"", rc.to_string_lossy());
    let engine = run_niu_with_env(&["-c", &load], &envs);
    assert_success(&engine, "engine bash-it load");
    // PS1 itself ends with a space: trim only newlines, not the payload.
    assert_eq!(
        stdout_text(&engine).trim_end_matches(['\r', '\n']),
        "base-readydemox> ",
        "enabled plugin ran and the theme set PS1"
    );
    let _ = &load;

    // Disable removes the entry; the loader no longer defines the plugin.
    assert_success(
        &run_niu_with_env(&["plugin", "disable", "base"], &envs),
        "disable base",
    );
    assert!(!entry.exists());
    // After the disable the loader no longer defines _base_fn; only the
    // theme remains (calling _base_fn now would be a command-not-found).
    let theme_only = format!(
        ". {}; printf '%s' \"$PS1\"",
        sandbox.home.join(".niubashrc").to_string_lossy()
    );
    let engine2 = run_niu_with_env(&["-c", &theme_only], &envs);
    assert_success(&engine2, "engine bash-it after disable");
    assert_eq!(
        stdout_text(&engine2).trim_end_matches(['\r', '\n']),
        "demox> ",
        "plugin gone, theme still active"
    );

    let _ = fs::remove_dir_all(&sandbox.home.parent().unwrap());
}

/// bash-completion is whole-source: the guarded rc block is the activation,
/// per-completion assets are informational.
#[test]
fn bash_completion_activates_as_a_whole_source() {
    let (sandbox, envs) = Sandbox::new("bash-completion");
    let add = run_niu_with_env(
        &[
            "plugin",
            "add",
            "bash-completion",
            "--path",
            &fixture("bash-completion").to_string_lossy(),
        ],
        &envs,
    );
    assert_success(&add, "plugin add bash-completion");
    // License honesty at the trust boundary (fetch-on-demand GPL).
    assert!(
        stdout_text(&add).contains("GPL-2.0-or-later"),
        "{}",
        stdout_text(&add)
    );
    assert_success(
        &run_niu_with_env(&["plugin", "trust", "bash-completion"], &envs),
        "trust",
    );
    assert_success(
        &run_niu_with_env(&["plugin", "enable", "bash-completion"], &envs),
        "whole-source enable",
    );
    let rc = rc_of(&sandbox);
    assert!(rc.contains("bash_completion\" ]"), "{rc}");

    // Individual completions stay informational.
    let refused = run_niu_with_env(&["plugin", "enable", "git"], &envs);
    assert!(!refused.status.success(), "asset enable must refuse");
    assert!(
        String::from_utf8_lossy(&refused.stderr).contains("activates as a whole source"),
        "{}",
        String::from_utf8_lossy(&refused.stderr)
    );

    // Engine proof: sourcing the rc runs the guarded loader.
    let load = format!(
        ". {}; printf '%s' \"$BASH_COMPLETION_STUB\"",
        sandbox.home.join(".niubashrc").to_string_lossy()
    );
    let engine = run_niu_with_env(&["-c", &load], &envs);
    assert_success(&engine, "engine bash-completion load");
    assert_eq!(stdout_text(&engine).trim_end(), "1");

    let _ = fs::remove_dir_all(&sandbox.home.parent().unwrap());
}

/// Graded trust: hash lock by default; the local-signature tier pins the
/// exact tree and re-gates updates until re-signed.
#[test]
fn trust_tiers_from_hash_lock_to_local_signature() {
    let (sandbox, envs) = Sandbox::new("trust-tiers");
    assert_success(
        &run_niu_with_env(
            &[
                "plugin",
                "add",
                "oh-my-bash",
                "--path",
                &fixture("oh-my-bash").to_string_lossy(),
            ],
            &envs,
        ),
        "add",
    );
    // Default tier on trust.
    assert_success(
        &run_niu_with_env(&["plugin", "trust", "oh-my-bash"], &envs),
        "trust",
    );
    let list = run_niu_with_env(&["plugin", "list"], &envs);
    assert!(
        stdout_text(&list).contains("trusted: checksum"),
        "{}",
        stdout_text(&list)
    );

    // Signature tier: verify passes and the listing shows it.
    assert_success(
        &run_niu_with_env(&["plugin", "source", "sign", "oh-my-bash"], &envs),
        "sign",
    );
    let sign_list = run_niu_with_env(&["plugin", "list"], &envs);
    assert!(
        stdout_text(&sign_list).contains("trusted: local"),
        "{}",
        stdout_text(&sign_list)
    );
    assert_success(
        &run_niu_with_env(&["plugin", "source", "verify", "oh-my-bash"], &envs),
        "verify signed",
    );

    // Tampering: trust-gate repairs refuse, doctor-visible via verify.
    let theme = sandbox
        .sources_root
        .join("oh-my-bash/themes/robbyrussell/robbyrussell.theme.sh");
    fs::write(&theme, "PS1='tampered'\n").unwrap();
    let verify_bad = run_niu_with_env(&["plugin", "source", "verify", "oh-my-bash"], &envs);
    assert!(
        !verify_bad.status.success(),
        "tampered tree must fail verify"
    );

    // The local signing key lives in the (overridden) sources root.
    assert!(sandbox.sources_root.join("signing-key.ed25519").is_file());

    let _ = fs::remove_dir_all(&sandbox.home.parent().unwrap());
}

/// Degraded sources surface honestly everywhere and restore repairs them
/// (for git origins; local snapshots explain instead).
#[test]
fn degraded_source_is_reported_and_local_restore_explains() {
    let (sandbox, envs) = Sandbox::new("degraded-cli");
    assert_success(
        &run_niu_with_env(
            &[
                "plugin",
                "add",
                "oh-my-bash",
                "--path",
                &fixture("oh-my-bash").to_string_lossy(),
            ],
            &envs,
        ),
        "add",
    );
    fs::remove_dir_all(sandbox.sources_root.join("oh-my-bash")).unwrap();

    let list = run_niu_with_env(&["plugin", "list"], &envs);
    assert_success(&list, "list degraded");
    let list_out = stdout_text(&list);
    assert!(list_out.contains("degraded"), "{list_out}");
    assert!(
        list_out.contains("niu plugin restore oh-my-bash"),
        "{list_out}"
    );

    // doctor names the fallback explicitly (iron law 2).
    let doctor = run_niu_with_env(&["doctor"], &envs);
    assert_success(&doctor, "doctor with degraded source");
    let doctor_out = stdout_text(&doctor);
    assert!(doctor_out.contains("source degraded"), "{doctor_out}");
    assert!(doctor_out.contains("fallback active"), "{doctor_out}");

    // Local snapshots cannot rebuild from upstream: restore explains.
    let restore = run_niu_with_env(&["plugin", "restore", "oh-my-bash"], &envs);
    assert!(!restore.status.success(), "local restore must fail");
    assert!(
        String::from_utf8_lossy(&restore.stderr).contains("local directory snapshot"),
        "{}",
        String::from_utf8_lossy(&restore.stderr)
    );

    // clean removes nothing here (the tree is gone, not orphaned).
    let clean = run_niu_with_env(&["plugin", "clean"], &envs);
    assert_success(&clean, "clean");
    assert!(stdout_text(&clean).contains("nothing to clean"));

    let _ = fs::remove_dir_all(&sandbox.home.parent().unwrap());
}

/// `niu plugin add owner/repo` expands the GitHub shorthand (resolution
/// only — offline: the expanded URL is asserted from the error path of a
/// fetch against a non-existent path-less target would need the network,
/// so the expansion is asserted through the catalog id instead).
#[test]
fn plugin_add_catalog_id_resolves_and_hints_the_next_steps() {
    let (sandbox, envs) = Sandbox::new("add-catalog");
    let add = run_niu_with_env(
        &[
            "plugin",
            "add",
            "bash-it",
            "--path",
            &fixture("bash-it").to_string_lossy(),
        ],
        &envs,
    );
    assert_success(&add, "catalog add");
    let out = stdout_text(&add);
    assert!(out.contains("license"), "{out}");
    assert!(out.contains("niu plugin trust bash-it"), "{out}");
    assert!(out.contains("niu plugin enable bash-it"), "{out}");

    // The curated catalog is visible in discover with official origins.
    let discover = run_niu_with_env(&["plugin", "discover"], &envs);
    assert_success(&discover, "discover");
    let discover_out = stdout_text(&discover);
    assert!(
        discover_out.contains("niu plugin add oh-my-bash"),
        "{discover_out}"
    );
    assert!(
        discover_out.contains("https://github.com/ohmybash/oh-my-bash.git"),
        "{discover_out}"
    );
    // bash-it and bash-completion are curated too; the GPL one says so.
    assert!(discover_out.contains("bash-completion"), "{discover_out}");
    assert!(discover_out.contains("GPL-2.0-or-later"), "{discover_out}");

    let _ = fs::remove_dir_all(&sandbox.home.parent().unwrap());
}

/// The setup wizard writes a journal and the undo contract holds for the
/// non-interactive path (iron law 3).
#[test]
fn setup_writes_a_journal_for_the_noninteractive_run() {
    let (sandbox, envs) = Sandbox::new("setup-journal");
    let setup = run_niu_with_env(&["setup"], &envs);
    assert_success(&setup, "non-interactive setup");
    let journal_path = sandbox.home.join(".niubash").join("setup-journal.toml");
    let journal = fs::read_to_string(&journal_path).expect("journal written");
    assert!(journal.contains("niubash:setup-journal@0.1.0"), "{journal}");
    assert!(journal.contains("preset = 'minimal'"), "{journal}");
    assert!(journal.contains("applied_at = "), "{journal}");

    let _ = fs::remove_dir_all(&sandbox.home.parent().unwrap());
}

/// Run `niu -i` with piped stdin under sandbox envs (interactive rc loads,
/// commands stream from the pipe; prompts go to stderr).
fn run_niu_i_with_stdin(stdin_data: &str, envs: &[(&str, String)]) -> Output {
    use std::io::Write;
    use std::process::Stdio;
    let mut command = Command::new(niu_binary());
    command
        .arg("-i")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (key, value) in envs {
        command.env(key, value);
    }
    let mut child = command
        .spawn()
        .unwrap_or_else(|err| panic!("failed to spawn niu -i: {err}"));
    if let Some(stdin) = child.stdin.as_mut() {
        stdin
            .write_all(stdin_data.as_bytes())
            .and_then(|()| stdin.flush())
            .expect("write niu stdin");
    }
    drop(child.stdin.take());
    child
        .wait_with_output()
        .unwrap_or_else(|err| panic!("failed to wait for niu -i: {err}"))
}

/// wt44/niu365 (owner ruling 2026-10-02: loader fidelity, no shim layer) —
/// an oh-my-bash framework asset is enabled ONLY through its native
/// loader: `niu plugin enable` writes the `plugins=(…)` array, the guarded
/// rc-block loader sources oh-my-bash.sh, and the framework itself both
/// consumes the array and provides its lib. The positive half proves the
/// whole chain leaves the plugin's functions usable in an interactive
/// session; the negative half proves no enable path bypasses the loader
/// (no direct source of the asset file anywhere in the managed block).
#[test]
fn omb_framework_assets_enable_only_through_the_native_loader() {
    let (sandbox, envs) = Sandbox::new("omb-loader-fidelity");
    let add = run_niu_with_env(
        &[
            "plugin",
            "add",
            "oh-my-bash",
            "--path",
            &fixture("oh-my-bash").to_string_lossy(),
        ],
        &envs,
    );
    assert_success(&add, "plugin add oh-my-bash --path");
    assert_success(
        &run_niu_with_env(&["plugin", "trust", "oh-my-bash"], &envs),
        "plugin trust oh-my-bash",
    );
    let enable = run_niu_with_env(&["plugin", "enable", "oh-my-bash/bashmarks"], &envs);
    assert_success(&enable, "enable oh-my-bash/bashmarks");

    // Negative half: the managed block selects the asset through the
    // framework's own array and sources only the framework loader — never
    // the asset file directly.
    let rc = rc_of(&sandbox);
    assert!(rc.contains("plugins=('bashmarks')"), "{rc}");
    assert!(
        rc.contains(". \"$OSH/oh-my-bash.sh\""),
        "the block sources the native loader: {rc}"
    );
    assert!(
        !rc.contains("plugins/bashmarks"),
        "the block must not bypass the loader by sourcing the asset file: {rc}"
    );
    assert!(
        !rc.contains("_omb_"),
        "the block defines no framework-lib shim (owner ruling: no reimplementation): {rc}"
    );

    // Positive half: interactive session — the rc runs the guarded loader,
    // the loader consumes plugins=('bashmarks') like the real one, and the
    // plugin's functions are usable (dispatcher + save/list roundtrip).
    let script = ". \"$HOME/.niubashrc\"\n\
                  declare -F bm _bashmarks_save\n\
                  bm -h\n\
                  bm -a mark\n\
                  bm -l\n\
                  exit\n";
    let output = run_niu_i_with_stdin(script, &envs);
    assert_success(&output, "interactive bashmarks session");
    let stdout = stdout_text(&output);
    for expected in ["bm\n", "_bashmarks_save\n", "USAGE:\n", "mark="] {
        assert!(
            stdout.contains(expected),
            "missing {expected:?} in stdout:\n{stdout}"
        );
    }
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(
        !stderr.contains("command not found"),
        "loader-path session leaked a command-not-found:\n{stderr}"
    );

    let _ = fs::remove_dir_all(&sandbox.home.parent().unwrap());
}

/// The independent-asset half of the same ruling: a plugin file with no
/// framework-lib dependency is legally sourceable directly (the same
/// `niu -c '. <file>'` engine proof the git fixture uses), while a file
/// that DOES call framework-lib functions fails exactly like it would
/// under GNU bash when sourced manually — niubash adds no compat layer,
/// and the errors are the fidelity contract, not a bug.
#[test]
fn independent_assets_source_directly_and_framework_deps_stay_honest() {
    let (sandbox, envs) = Sandbox::new("omb-independent-vs-framework");
    let add = run_niu_with_env(
        &[
            "plugin",
            "add",
            "oh-my-bash",
            "--path",
            &fixture("oh-my-bash").to_string_lossy(),
        ],
        &envs,
    );
    assert_success(&add, "plugin add oh-my-bash --path");
    assert_success(
        &run_niu_with_env(&["plugin", "trust", "oh-my-bash"], &envs),
        "plugin trust oh-my-bash",
    );

    // Lib-free asset (the fixture bashmarks core): manual direct source
    // works — this is the sanctioned independent path.
    let independent = sandbox
        .sources_root
        .join("oh-my-bash/plugins/bashmarks/bashmarks.plugin.sh");
    let script = format!(". {}; declare -F bm", independent.to_string_lossy());
    let direct = run_niu_with_env(&["-c", &script], &envs);
    assert_success(&direct, "direct source of a lib-free asset");
    assert_eq!(stdout_text(&direct).trim_end(), "bm");

    // Framework-lib-dependent asset: a stand-in shaped like the REAL
    // bashmarks deprecated-interface block, sourced manually without the
    // loader. GNU bash reports `_omb_deprecate_declare: command not
    // found` and continues; niubash must do the same (no shim, no silent
    // success, no extra behavior).
    let framework_dep = sandbox.home.join("framework-dep.plugin.sh");
    fs::write(
        &framework_dep,
        "#! bash oh-my-bash.module\n\
         _omb_deprecate_declare  20000 SDIRS BASHMARKS_SDIRS sync\n\
         _omb_deprecate_function 20000 _l _bashmarks_list_names\n\
         function _bashmarks_list_names { :; }\n",
    )
    .unwrap();
    let script = format!(
        ". {} 2>&1; printf 'sdirs-fn:%s\\n' \"$(declare -F _bashmarks_list_names >/dev/null && echo yes)\"
",
        framework_dep.to_string_lossy()
    );
    let manual = run_niu_with_env(&["-c", &script], &envs);
    assert_success(&manual, "manual source of a framework-dependent file");
    let manual_out = stdout_text(&manual);
    assert!(
        manual_out.contains("_omb_deprecate_declare: command not found"),
        "GNU-fidelity: the missing framework lib must be reported, got:\n{manual_out}"
    );
    assert!(
        manual_out.contains("_omb_deprecate_function: command not found"),
        "GNU-fidelity: every missing lib call is reported, got:\n{manual_out}"
    );
    assert!(
        manual_out.contains("sdirs-fn:yes"),
        "like GNU bash, execution continues past the missing-lib errors:\n{manual_out}"
    );

    let _ = fs::remove_dir_all(&sandbox.home.parent().unwrap());
}
