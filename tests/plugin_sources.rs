//! Binary-level integration smoke for external plugin-manager sources
//! (oh-my-bash loader adapter; docs/planning/oh-my-niu-ecosystem.md §11-§12).
//!
//! Vertical covered: `niu plugin source add` (fetch gate, untrusted) →
//! `trust` (execution gate) → catalog exposure (`niu plugin themes`) →
//! theme load through the niubash engine from the adapter-installed tree →
//! `verify` (tree checksum) → `remove` → built-in fallback verified when
//! the source is absent.
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

fn base_niubash_command() -> Command {
    Command::new(niu_binary())
}

fn run_niu_with_env(args: &[&str], envs: &[(&str, PathBuf)]) -> Output {
    let mut command = base_niubash_command();
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

fn temp_dir(name: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!("niubash-{name}-{}-{nanos}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// The vendored oh-my-bash-shaped fixture tree (LF, pinned by .gitattributes;
/// layout mirrors D:/repo/rubash/target-ecosys/repos/oh-my-bash).
fn omb_fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("sources")
        .join("oh-my-bash")
}

#[test]
fn oh_my_bash_source_add_trust_load_and_fallback_lifecycle() {
    let temp = temp_dir("plugin-source-omb");
    let root = temp.join("sources");
    let fixture = omb_fixture();
    let envs = [("NIU_PLUGIN_SOURCES_ROOT", root.clone())];
    let fixture_str = fixture.to_string_lossy().into_owned();

    // Baseline: no sources, catalog falls back to the built-in layers.
    let empty = run_niu_with_env(&["plugin", "source", "list"], &envs);
    assert_success(&empty, "plugin source list empty");
    assert!(
        stdout_text(&empty).contains("no sources installed"),
        "{}",
        stdout_text(&empty)
    );

    // Fetch gate: install is untrusted; assets must not activate yet.
    let add = run_niu_with_env(&["plugin", "source", "add", &fixture_str], &envs);
    assert_success(&add, "plugin source add");
    let add_out = stdout_text(&add);
    assert!(
        add_out.contains("Installed source 'oh-my-bash'"),
        "{add_out}"
    );
    assert!(
        add_out.contains("Trust boundary") && add_out.contains("nothing is sourced yet"),
        "{add_out}"
    );
    assert!(
        add_out.contains("niu plugin source trust oh-my-bash"),
        "{add_out}"
    );

    let listed_untrusted = run_niu_with_env(&["plugin", "source", "list"], &envs);
    assert_success(&listed_untrusted, "plugin source list untrusted");
    assert!(
        stdout_text(&listed_untrusted).contains("untrusted"),
        "{}",
        stdout_text(&listed_untrusted)
    );
    // The built-in theme catalog retired (niubash#145); the untrusted
    // state itself is the gate and stays visible in the source listing.
    assert!(
        stdout_text(&listed_untrusted).contains("untrusted"),
        "untrusted state must be listed: {}",
        stdout_text(&listed_untrusted)
    );

    // Execution gate: trust activates the source's assets in the catalog.
    let trust = run_niu_with_env(&["plugin", "source", "trust", "oh-my-bash"], &envs);
    assert_success(&trust, "plugin source trust");
    let trust_out = stdout_text(&trust);
    assert!(
        trust_out.contains("license")
            && trust_out.contains("MIT")
            && trust_out.contains("verified"),
        "{trust_out}"
    );

    let listed_ready = run_niu_with_env(&["plugin", "source", "list"], &envs);
    assert_success(&listed_ready, "plugin source list ready");
    assert!(
        stdout_text(&listed_ready).contains("ready"),
        "{}",
        stdout_text(&listed_ready)
    );

    // Trusted sources expose their theme assets through the wizard gallery
    // and the loader snippet; the retired catalog surface is gone.
    // niubash#179 L05-1: discover must enumerate the theme layer, not just
    // the sources — the wizard's notes promise "sources & themes".
    let discover = run_niu_with_env(&["plugin", "discover"], &envs);
    assert_success(&discover, "plugin discover after trust");
    let discover_out = stdout_text(&discover);
    assert!(discover_out.contains("oh-my-bash"), "{discover_out}");
    assert!(
        discover_out.contains("Themes (from trusted sources)"),
        "discover must list the theme layer: {discover_out}"
    );
    assert!(
        discover_out.contains("robbyrussell"),
        "discover must name trusted-source themes: {discover_out}"
    );

    // Load the vendored theme through the adapter-installed tree under the
    // niubash engine (script face; interactive activation is gated on
    // rubash#251 per §12.6). Source order mirrors the loader: lib, theme.
    let installed_theme = root
        .join("oh-my-bash")
        .join("themes")
        .join("robbyrussell")
        .join("robbyrussell.theme.sh");
    let installed_lib = root.join("oh-my-bash").join("lib").join("utils.sh");
    let load_script = format!(
        ". {}; . {}; printf '%s' \"$PS1\"",
        installed_lib.to_string_lossy(),
        installed_theme.to_string_lossy()
    );
    let load = run_niu_with_env(&["-c", &load_script], &envs);
    assert_success(&load, "engine theme load");
    let load_out = stdout_text(&load);
    assert_eq!(
        load_out.trim_end(),
        "loading robbyrussell\n➜ prompt-robbyrussell",
        "theme must compose PS1 under the niubash engine"
    );

    // Non-interactive loader safety: the oh-my-bash.sh interactive guard
    // makes the loader a no-op in -c mode (PS1 untouched).
    let osh = root.join("oh-my-bash");
    let guard_script = format!(
        "PS1=before; export OSH={}; . \"{}/oh-my-bash.sh\"; printf '%s' \"$PS1\"",
        osh.to_string_lossy(),
        osh.to_string_lossy()
    );
    let guard = run_niu_with_env(&["-c", &guard_script], &envs);
    assert_success(&guard, "loader guard");
    assert_eq!(stdout_text(&guard).trim_end(), "before");

    // Checksum verification (§12.3).
    let verify = run_niu_with_env(&["plugin", "source", "verify", "oh-my-bash"], &envs);
    assert_success(&verify, "plugin source verify");
    assert!(
        stdout_text(&verify).contains("Verified source 'oh-my-bash'"),
        "{}",
        stdout_text(&verify)
    );

    // Uninstall, then prove the built-in fallback: the catalog returns to
    // the compiled/bundle layers and theme resolution still works.
    let remove = run_niu_with_env(&["plugin", "source", "remove", "oh-my-bash"], &envs);
    assert_success(&remove, "plugin source remove");
    // The built-in theme catalog retired (niubash#145); the removed source
    // must simply be gone from the source listing.
    let sources_after = run_niu_with_env(&["plugin", "source", "list"], &envs);
    assert_success(&sources_after, "plugin source list after remove");
    assert!(
        !stdout_text(&sources_after).contains("oh-my-bash"),
        "removed source must be gone from the listing: {}",
        stdout_text(&sources_after)
    );

    let _ = fs::remove_dir_all(&temp);
}

#[test]
fn plugin_source_add_rejects_unknown_layout_and_bad_checksum() {
    let temp = temp_dir("plugin-source-omb-errors");
    let root = temp.join("sources");
    let envs = [("NIU_PLUGIN_SOURCES_ROOT", root.clone())];

    // Not a known plugin-manager tree.
    let random = temp.join("random-tree");
    fs::create_dir_all(&random).unwrap();
    fs::write(random.join("notes.txt"), "not a manager\n").unwrap();
    let bad = run_niu_with_env(
        &["plugin", "source", "add", &random.to_string_lossy()],
        &envs,
    );
    assert!(!bad.status.success(), "unknown layout must fail");
    let bad_out = format!(
        "{}{}",
        stdout_text(&bad),
        String::from_utf8_lossy(&bad.stderr)
    );
    assert!(
        bad_out.contains("no supported plugin manager") && bad_out.contains("oh-my-bash"),
        "{bad_out}"
    );

    // Checksum mismatch aborts and installs nothing.
    let fixture = omb_fixture();
    let wrong = run_niu_with_env(
        &[
            "plugin",
            "source",
            "add",
            &fixture.to_string_lossy(),
            "--checksum",
            "deadbeef",
        ],
        &envs,
    );
    assert!(!wrong.status.success(), "checksum mismatch must fail");
    assert!(
        String::from_utf8_lossy(&wrong.stderr).contains("checksum mismatch"),
        "{}",
        String::from_utf8_lossy(&wrong.stderr)
    );
    let listed = run_niu_with_env(&["plugin", "source", "list"], &envs);
    assert!(
        stdout_text(&listed).contains("no sources installed"),
        "{}",
        stdout_text(&listed)
    );

    let _ = fs::remove_dir_all(&temp);
}

/// `niu plugin discover` is the dry ecosystem overview: it shows installed
/// sources with their state and the managers that are not installed yet,
/// without writing anything (not even the registry file).
#[test]
fn plugin_discover_is_read_only_and_lists_available_managers() {
    let temp = temp_dir("plugin-discover");
    let root = temp.join("sources");
    let envs = [("NIU_PLUGIN_SOURCES_ROOT", root.clone())];

    let out = run_niu_with_env(&["plugin", "discover"], &envs);
    assert_success(&out, "plugin discover empty");
    let text = stdout_text(&out);
    assert!(text.contains("read-only"), "{text}");
    assert!(text.contains("(none installed)"), "{text}");
    // niubash#179 L05-1: the theme layer is enumerated (empty note here)
    // so the wizard's "browse sources & themes" promise stays true.
    assert!(
        text.contains("Themes (from trusted sources)") && text.contains("(none yet"),
        "{text}"
    );
    // oh-my-bash is a known manager that is not installed yet: the hint
    // shows the add command the user could run — nothing runs on its own.
    assert!(text.contains("oh-my-bash"), "{text}");
    assert!(text.contains("niu plugin add oh-my-bash"), "{text}");
    assert!(
        text.contains("https://github.com/ohmybash/oh-my-bash.git"),
        "{text}"
    );
    assert!(
        text.contains("nothing is installed, sourced, or changed"),
        "{text}"
    );
    // Read-only proof: no registry was created by a listing command.
    assert!(
        !root.join("registry.toml").exists(),
        "discover must not write the registry"
    );

    // With a source installed (untrusted), discover surfaces the state.
    let fixture = omb_fixture();
    run_niu_with_env(
        &["plugin", "source", "add", &fixture.to_string_lossy()],
        &envs,
    );
    let out = run_niu_with_env(&["plugin", "discover"], &envs);
    assert_success(&out, "plugin discover untrusted");
    let text = stdout_text(&out);
    assert!(text.contains("untrusted"), "{text}");
    assert!(
        !text.contains("niu plugin add oh-my-bash"),
        "installed managers leave the available section: {text}"
    );

    let _ = fs::remove_dir_all(&temp);
}

/// The non-interactive `niu setup` contract stays deterministic: minimal
/// preset applied, no wizard-answer markers written without a question.
#[test]
fn setup_noninteractive_stays_deterministic_and_records_no_answers() {
    let temp = temp_dir("setup-noninteractive");
    let home = temp.join("home");
    fs::create_dir_all(&home).unwrap();
    let envs = [
        ("HOME", home.clone()),
        ("USERPROFILE", home.clone()),
        ("NIU_PLUGIN_SOURCES_ROOT", temp.join("sources")),
    ];
    let out = run_niu_with_env(&["setup"], &envs);
    assert_success(&out, "non-interactive setup");
    let rc = fs::read_to_string(home.join(".niubashrc")).expect("rc written");
    // Clean rc (niubash#145): the retired stack variables are absent.
    assert!(!rc.contains("NIU_THEME="), "{rc}");
    assert!(!rc.contains("NIU_PLUGINS="), "{rc}");
    assert!(!rc.contains("NIU_DISABLE_DEFAULT_PLUGINS"), "{rc}");
    assert!(rc.contains("USERPROFILE"), "{rc}");
    assert!(
        home.join(".niubash").join(".setup-done").is_file(),
        "setup-done marker must exist"
    );
    // No question was asked, so no lasting answer (niu-git nag marker) may
    // be recorded.
    assert!(
        !home.join(".niubash").join("wizard-answers.toml").is_file(),
        "wizard answers must not be written without an explicit pick"
    );
    let _ = fs::remove_dir_all(&temp);
}

/// niubash#179 L05-3: a *gutted* trusted source (directory present,
/// contents emptied) still lists as "ready" — the checksum state is the
/// only remaining truth — but `niu plugin list` and `niu plugin discover`
/// must name the verb that reports it instead of silently showing zero
/// assets.
#[test]
fn gutted_trusted_source_names_the_verify_verb() {
    let temp = temp_dir("plugin-gutted");
    let root = temp.join("sources");
    let envs = [("NIU_PLUGIN_SOURCES_ROOT", root.clone())];
    let fixture = omb_fixture();

    let add = run_niu_with_env(
        &["plugin", "source", "add", &fixture.to_string_lossy()],
        &envs,
    );
    assert_success(&add, "plugin source add");
    let trust = run_niu_with_env(&["plugin", "source", "trust", "oh-my-bash"], &envs);
    assert_success(&trust, "plugin source trust");

    // Gut the tree: the directory survives, its contents do not.
    let tree = root.join("oh-my-bash");
    assert!(tree.is_dir(), "installed tree must exist");
    for entry in fs::read_dir(&tree).expect("tree readable") {
        let entry = entry.expect("tree entry readable");
        if entry.file_type().expect("entry type").is_dir() {
            fs::remove_dir_all(entry.path()).expect("remove subtree");
        } else {
            fs::remove_file(entry.path()).expect("remove file");
        }
    }

    let list = run_niu_with_env(&["plugin", "list"], &envs);
    assert_success(&list, "plugin list gutted");
    let list_text = stdout_text(&list);
    assert!(
        list_text.contains("ready"),
        "gutted tree still registers as ready: {list_text}"
    );
    assert!(
        list_text.contains("niu plugin source verify oh-my-bash"),
        "plugin list must name the verify verb for a gutted tree: {list_text}"
    );

    let discover = run_niu_with_env(&["plugin", "discover"], &envs);
    assert_success(&discover, "plugin discover gutted");
    assert!(
        stdout_text(&discover).contains("niu plugin source verify oh-my-bash"),
        "discover must name the verify verb for a gutted tree: {}",
        stdout_text(&discover)
    );

    let _ = fs::remove_dir_all(&temp);
}
