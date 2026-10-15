//! Declarative-spec pinning tests (design §14.6.3, wt46/anyplug):
//!
//! * `niu plugin sync` materializes the rc managed block from the spec
//!   **idempotently** — an unchanged spec yields a byte-identical rc and a
//!   clean report; `--bootstrap` is always a silent compatibility no-op;
//! * merge semantics: entries the spec dropped leave the block, hand-added
//!   OMB array entries survive, a hand-set theme survives a spec that
//!   declares none;
//! * cleanup honesty: installed-but-undeclared sources get a suggestion,
//!   never an auto-delete; `--prune` is the explicit confirm;
//! * CLI = sugar: `niu plugin add` + `niu plugin enable` produce the same
//!   spec file and the same rc block as hand-writing the spec and syncing.
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

fn stderr_text(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
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

struct Sandbox {
    home: PathBuf,
    sources_root: PathBuf,
    envs: Vec<(&'static str, String)>,
}

impl Sandbox {
    fn new(label: &str) -> Self {
        let temp = temp_dir(label);
        let home = temp.join("home");
        let sources_root = temp.join("sources");
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(home.join(".niubash")).unwrap();
        let envs = vec![
            ("HOME", home.to_string_lossy().into_owned()),
            ("USERPROFILE", home.to_string_lossy().into_owned()),
            (
                "NIU_PLUGIN_SOURCES_ROOT",
                sources_root.to_string_lossy().into_owned(),
            ),
            (
                "NIU_PLUGIN_SPEC",
                home.join(".niubash")
                    .join("plugins.toml")
                    .to_string_lossy()
                    .into_owned(),
            ),
        ];
        Sandbox {
            home,
            sources_root,
            envs,
        }
    }

    fn rc(&self) -> String {
        fs::read_to_string(self.home.join(".niubashrc")).unwrap_or_default()
    }

    fn spec_path(&self) -> PathBuf {
        self.home.join(".niubash").join("plugins.toml")
    }

    fn spec(&self) -> String {
        fs::read_to_string(self.spec_path()).unwrap_or_default()
    }
}

/// Sync materializes idempotently: an unchanged spec regenerates the rc
/// block byte-identically, reports clean, and the bootstrap compatibility
/// no-op prints nothing. Spec drops remove entries; hand-added array items and a
/// hand-set theme survive.
#[test]
fn sync_is_idempotent_bootstrap_quiet_and_merges_honestly() {
    let sandbox = Sandbox::new("spec-idempotent");
    let origin = fixture("oh-my-bash").to_string_lossy().replace('\\', "/");

    // Hand-written spec: local fixture path target, pinned manager id.
    fs::write(
        sandbox.spec_path(),
        format!(
            "schema = \"niubash:plugin-spec@0.1.0\"\n\
             \n\
             [[sources]]\n\
             target = '{origin}'\n\
             id = 'oh-my-bash'\n\
             theme = 'agnoster'\n\
             enable = ['git', 'cargo']\n"
        ),
    )
    .unwrap();

    // Sync 1: install (fetch gate only) — lands untrusted, nothing
    // activated, and the exact trust command is printed.
    let first = run_niu_with_env(&["plugin", "sync"], &sandbox.envs);
    assert_success(&first, "sync 1");
    let first_out = stdout_text(&first);
    assert!(
        first_out.contains("awaiting-trust") && first_out.contains("niu plugin trust oh-my-bash"),
        "{first_out}"
    );
    assert!(sandbox.rc().is_empty(), "untrusted activates nothing");
    assert_success(
        &run_niu_with_env(&["plugin", "trust", "oh-my-bash"], &sandbox.envs),
        "trust",
    );

    // Sync 2: materialize from the spec.
    let second = run_niu_with_env(&["plugin", "sync"], &sandbox.envs);
    assert_success(&second, "sync 2");
    let rc_after = sandbox.rc();
    assert!(rc_after.contains("OSH_THEME='agnoster'"), "{rc_after}");
    assert!(rc_after.contains("plugins=('git')"), "{rc_after}");
    assert!(rc_after.contains("aliases=('cargo')"), "{rc_after}");

    // Sync 3 (the idempotency pin): byte-identical rc, all rows unchanged,
    // report says in sync.
    let third = run_niu_with_env(&["plugin", "sync"], &sandbox.envs);
    assert_success(&third, "sync 3");
    assert_eq!(sandbox.rc(), rc_after, "rc must be byte-identical");
    assert!(
        stdout_text(&third).contains("in sync"),
        "{}",
        stdout_text(&third)
    );

    // Bootstrap is a silent no-op (stdout AND stderr).
    let boot = run_niu_with_env(&["plugin", "sync", "--bootstrap"], &sandbox.envs);
    assert_success(&boot, "bootstrap clean");
    assert!(
        stdout_text(&boot).is_empty() && stderr_text(&boot).is_empty(),
        "clean bootstrap prints nothing: stdout={} stderr={}",
        stdout_text(&boot),
        stderr_text(&boot)
    );

    // Merge semantics: hand-add an entry to the rc array and drop 'cargo'
    // from the spec. The next sync removes spec-dropped cargo and keeps
    // the hand-added g2; a spec without a theme key keeps the hand-set one.
    let hand = rc_after.replace("plugins=('git')", "plugins=('git' 'g2')");
    assert!(
        hand != rc_after,
        "hand edit must land (block shape changed?)"
    );
    fs::write(sandbox.home.join(".niubashrc"), hand).unwrap();
    fs::write(
        sandbox.spec_path(),
        format!(
            "schema = \"niubash:plugin-spec@0.1.0\"\n\
             \n\
             [[sources]]\n\
             target = '{origin}'\n\
             id = 'oh-my-bash'\n\
             enable = ['git']\n"
        ),
    )
    .unwrap();
    let merged = run_niu_with_env(&["plugin", "sync"], &sandbox.envs);
    assert_success(&merged, "merge sync");
    let rc = sandbox.rc();
    assert!(
        rc.contains("plugins=('git' 'g2')"),
        "spec-dropped cargo gone, hand-added g2 kept: {rc}"
    );
    assert!(!rc.contains("aliases="), "spec-dropped alias removed: {rc}");
    assert!(
        rc.contains("OSH_THEME='agnoster'"),
        "hand-set theme survives a spec that declares none: {rc}"
    );

    // The survival pin: hand-added entries must stay hand-added — they
    // survive not just the sync that first sees them but every later sync
    // of an unchanged spec (they must never be absorbed into the spec's
    // own recorded selection and then dropped by a subsequent change).
    let settled = run_niu_with_env(&["plugin", "sync"], &sandbox.envs);
    assert_success(&settled, "settling sync");
    let rc = sandbox.rc();
    assert!(
        rc.contains("plugins=('git' 'g2')"),
        "hand-added g2 survives repeated syncs: {rc}"
    );
    assert!(
        stdout_text(&settled).contains("in sync"),
        "{}",
        stdout_text(&settled)
    );

    let _ = fs::remove_dir_all(sandbox.home.parent().unwrap());
}

/// The CLI is sugar over the spec: `niu plugin add` + `niu plugin enable`
/// reach the exact spec file and rc block that hand-writing the spec and
/// syncing produces (CLI-spec consistency pin).
#[test]
fn cli_sugar_matches_the_handwritten_spec_byte_for_byte() {
    let origin = fixture("oh-my-bash").to_string_lossy().replace('\\', "/");

    // Lane A: pure CLI.
    let cli = Sandbox::new("spec-cli");
    let add = run_niu_with_env(
        &["plugin", "add", "oh-my-bash", "--path", &origin],
        &cli.envs,
    );
    assert_success(&add, "cli add");
    assert_success(
        &run_niu_with_env(&["plugin", "trust", "oh-my-bash"], &cli.envs),
        "cli trust",
    );
    for target in ["git", "agnoster"] {
        assert_success(
            &run_niu_with_env(&["plugin", "enable", target], &cli.envs),
            &format!("cli enable {target}"),
        );
    }

    // Lane B: hand-written spec + sync.
    let hand = Sandbox::new("spec-hand");
    fs::write(
        hand.spec_path(),
        format!(
            "schema = \"niubash:plugin-spec@0.1.0\"\n\
             \n\
             [[sources]]\n\
             target = '{origin}'\n\
             id = 'oh-my-bash'\n\
             theme = 'agnoster'\n\
             enable = ['git']\n"
        ),
    )
    .unwrap();
    let sync1 = run_niu_with_env(&["plugin", "sync"], &hand.envs);
    assert_success(&sync1, "hand sync 1");
    assert_success(
        &run_niu_with_env(&["plugin", "trust", "oh-my-bash"], &hand.envs),
        "hand trust",
    );
    let sync2 = run_niu_with_env(&["plugin", "sync"], &hand.envs);
    assert_success(&sync2, "hand sync 2");

    // The spec files match (the CLI path wrote the same declaration), and
    // the materialized rc blocks are byte-identical.
    assert_eq!(
        cli.spec(),
        hand.spec(),
        "CLI sugar must write the same spec"
    );
    assert_eq!(cli.rc(), hand.rc(), "materialized rc blocks must match");

    // Re-declaring the same source through the CLI is refused with the
    // spec location (idempotence of the front door).
    let dup = run_niu_with_env(
        &["plugin", "add", "oh-my-bash", "--path", &origin],
        &cli.envs,
    );
    assert!(
        !dup.status.success(),
        "duplicate declaration must be refused"
    );
    assert!(
        stderr_text(&dup).contains("already declared"),
        "{}",
        stderr_text(&dup)
    );

    let _ = fs::remove_dir_all(cli.home.parent().unwrap());
    let _ = fs::remove_dir_all(hand.home.parent().unwrap());
}

/// Cleanup honesty: a source installed imperatively (no spec) is suggested
/// for cleanup, never auto-deleted; `niu plugin sync --prune` is the
/// explicit confirm and removes tree + registry entry.
#[test]
fn undeclared_sources_are_suggested_then_pruned() {
    let sandbox = Sandbox::new("spec-prune");
    let origin = fixture("oh-my-bash");

    // Imperative install through the full source protocol: no spec file.
    let add = run_niu_with_env(
        &[
            "plugin",
            "source",
            "add",
            "oh-my-bash",
            "--path",
            &origin.to_string_lossy(),
        ],
        &sandbox.envs,
    );
    assert_success(&add, "imperative source add");
    assert!(
        !sandbox.spec_path().exists(),
        "imperative installs must not create a spec"
    );

    // Sync without a spec: legacy mode, honest report, no deletion.
    let legacy = run_niu_with_env(&["plugin", "sync"], &sandbox.envs);
    assert_success(&legacy, "legacy sync");
    let legacy_out = stdout_text(&legacy);
    assert!(
        legacy_out.contains("imperative mode") && legacy_out.contains("oh-my-bash"),
        "{legacy_out}"
    );
    assert!(
        sandbox
            .sources_root
            .join("oh-my-bash/oh-my-bash.sh")
            .is_file(),
        "never auto-deleted"
    );

    // An empty spec exists: same suggestion shape.
    fs::write(sandbox.spec_path(), "").unwrap();
    let hint = run_niu_with_env(&["plugin", "sync"], &sandbox.envs);
    assert_success(&hint, "hint sync");
    let hint_out = stdout_text(&hint);
    assert!(
        hint_out.contains("installed but not declared in the spec"),
        "{hint_out}"
    );
    assert!(
        hint_out.contains("--prune"),
        "the explicit confirm must be named: {hint_out}"
    );

    // --prune removes tree and registry entry.
    let prune = run_niu_with_env(&["plugin", "sync", "--prune"], &sandbox.envs);
    assert_success(&prune, "prune sync");
    assert!(
        stdout_text(&prune).contains("removed"),
        "{}",
        stdout_text(&prune)
    );
    assert!(
        !sandbox.sources_root.join("oh-my-bash").exists(),
        "tree pruned"
    );
    let registry =
        fs::read_to_string(sandbox.sources_root.join("registry.toml")).unwrap_or_default();
    assert!(
        !registry.contains("oh-my-bash"),
        "registry entry pruned: {registry}"
    );

    let _ = fs::remove_dir_all(sandbox.home.parent().unwrap());
}

/// The 1.3.0 imperative-mode dead end (F1/F3): with no spec, the quiet
/// startup form printed "installed but not declared" on EVERY terminal —
/// contradicting imperative mode's "nothing to reconcile". Startup must be
/// silent, and the interactive form must be actionable: it names the exact
/// migration verb (`niu plugin sync --adopt`) and the hand-write starter.
#[test]
fn no_spec_startup_is_silent_and_sync_suggests_adopt() {
    let sandbox = Sandbox::new("spec-nag");
    let origin = fixture("oh-my-bash");
    let add = run_niu_with_env(
        &["plugin", "source", "add", &origin.to_string_lossy()],
        &sandbox.envs,
    );
    assert_success(&add, "imperative install");
    assert!(
        !sandbox.spec_path().exists(),
        "imperative installs create no spec"
    );

    // Startup: silent (nothing to reconcile in imperative mode).
    let boot = run_niu_with_env(&["plugin", "sync", "--bootstrap"], &sandbox.envs);
    assert_success(&boot, "bootstrap sync");
    assert!(
        stdout_text(&boot).trim().is_empty(),
        "startup stdout must be empty: {}",
        stdout_text(&boot)
    );
    assert!(
        stderr_text(&boot).trim().is_empty(),
        "startup stderr must be empty: {}",
        stderr_text(&boot)
    );

    // Interactive: imperative-mode notice, the installed source, the
    // migration one-liner, and the hand-write starter block.
    let sync = run_niu_with_env(&["plugin", "sync"], &sandbox.envs);
    assert_success(&sync, "no-spec sync");
    let out = stdout_text(&sync);
    assert!(out.contains("imperative mode"), "{out}");
    assert!(out.contains("oh-my-bash"), "{out}");
    assert!(out.contains("niu plugin sync --adopt"), "{out}");
    assert!(out.contains("[[sources]]"), "starter block shown: {out}");
    assert!(
        out.contains("niu plugin add <target>"),
        "single-source advice still named: {out}"
    );

    let _ = fs::remove_dir_all(sandbox.home.parent().unwrap());
}

/// F2, the adoption contract: `niu plugin sync --adopt` declares installed
/// sources by snapshotting the live selection (theme + enabled assets), and
/// the adopted spec ROUND-TRIPS — a plain sync is a no-op (byte-stable rc
/// and spec) and the startup form is silent again.
#[test]
fn adopt_snapshots_the_live_state_and_round_trips() {
    let sandbox = Sandbox::new("spec-adopt");
    let origin = fixture("oh-my-bash");

    // The owner-shaped state: imperative install, explicit trust, live
    // theme + plugin — then the spec (written by the enable sugar) is gone,
    // as on every 1.3.0 wizard machine.
    let add = run_niu_with_env(
        &["plugin", "source", "add", &origin.to_string_lossy()],
        &sandbox.envs,
    );
    assert_success(&add, "imperative install");
    let trust = run_niu_with_env(&["plugin", "trust", "oh-my-bash"], &sandbox.envs);
    assert_success(&trust, "trust");
    let enable = run_niu_with_env(&["plugin", "enable", "oh-my-bash/git"], &sandbox.envs);
    assert_success(&enable, "enable plugin");
    let theme = run_niu_with_env(&["plugin", "enable", "agnoster"], &sandbox.envs);
    assert_success(&theme, "enable theme");
    let live_rc = sandbox.rc();
    assert!(live_rc.contains("OSH_THEME='agnoster'"), "{live_rc}");
    fs::remove_file(sandbox.spec_path()).unwrap();

    // --adopt declares it with the live snapshot.
    let adopt = run_niu_with_env(&["plugin", "sync", "--adopt"], &sandbox.envs);
    assert_success(&adopt, "adopt sync");
    let out = stdout_text(&adopt);
    assert!(out.contains("declared oh-my-bash"), "{out}");
    assert!(out.contains("adopted 1 source(s)"), "{out}");
    let spec = sandbox.spec();
    assert!(spec.contains("id = 'oh-my-bash'"), "{spec}");
    assert!(spec.contains("theme = 'agnoster'"), "{spec}");
    assert!(spec.contains("enable = ['git']"), "{spec}");
    assert_eq!(sandbox.rc(), live_rc, "adopt must not move the rc");

    // Round-trip: plain sync is a no-op — rc and spec byte-stable.
    let spec_once = sandbox.spec();
    let plain = run_niu_with_env(&["plugin", "sync"], &sandbox.envs);
    assert_success(&plain, "plain sync after adopt");
    assert!(
        stdout_text(&plain).contains("in sync"),
        "{}",
        stdout_text(&plain)
    );
    assert_eq!(sandbox.rc(), live_rc, "rc byte-identical");
    assert_eq!(sandbox.spec(), spec_once, "spec byte-identical");

    // And the startup form is silent again — the nag is gone for good.
    let boot = run_niu_with_env(&["plugin", "sync", "--bootstrap"], &sandbox.envs);
    assert_success(&boot, "bootstrap after adopt");
    assert!(
        stdout_text(&boot).trim().is_empty() && stderr_text(&boot).trim().is_empty(),
        "startup silent after adoption: {}{}",
        stdout_text(&boot),
        stderr_text(&boot)
    );

    let _ = fs::remove_dir_all(sandbox.home.parent().unwrap());
}

/// F4: `niu plugin add <target>` on an already-installed source DECLARES it
/// (printed as such) instead of dead-ending on the imperative refusal
/// ("source '...' is already registered; remove it first").
#[test]
fn plugin_add_on_an_installed_source_declares_it() {
    let sandbox = Sandbox::new("spec-add-adopt");
    let origin = fixture("oh-my-bash");
    let add = run_niu_with_env(
        &["plugin", "source", "add", &origin.to_string_lossy()],
        &sandbox.envs,
    );
    assert_success(&add, "imperative install");
    assert!(!sandbox.spec_path().exists());

    let declare = run_niu_with_env(&["plugin", "add", "oh-my-bash"], &sandbox.envs);
    assert_success(&declare, "add on an installed source must not fail");
    let out = stdout_text(&declare);
    assert!(out.contains("Declared"), "declared, not installed: {out}");
    assert!(out.contains("already installed"), "{out}");
    let spec = sandbox.spec();
    assert!(spec.contains("target = 'oh-my-bash'"), "{spec}");
    assert!(spec.contains("id = 'oh-my-bash'"), "{spec}");

    // Exactly one tree, untouched: the add declared, it did not fetch.
    let registry = fs::read_to_string(sandbox.sources_root.join("registry.toml")).unwrap();
    assert_eq!(registry.matches("[[sources]]").count(), 1, "{registry}");
    assert!(registry.contains("trusted = false"), "{registry}");

    let _ = fs::remove_dir_all(sandbox.home.parent().unwrap());
}

/// The `niu plugin add --checksum` pin is honored, not just echoed (wt83
/// #173): a mismatched tree is refused with nothing registered and no spec
/// entry left behind, and the tree's real checksum installs — the same
/// honesty `niu plugin source add --checksum` always had.
#[test]
fn plugin_add_honors_the_checksum_pin_and_rolls_back_on_failure() {
    let sandbox = Sandbox::new("add-checksum-pin");
    let origin = fixture("oh-my-bash");
    let origin_s = origin.to_string_lossy().into_owned();

    // A wrong checksum is refused: rc != 0, the reason names the mismatch,
    // the rolled-back declaration is admitted, and nothing is registered.
    let wrong = run_niu_with_env(
        &[
            "plugin",
            "add",
            "oh-my-bash",
            "--path",
            &origin_s,
            "--checksum",
            "deadbeef",
        ],
        &sandbox.envs,
    );
    assert!(!wrong.status.success(), "wrong pin must fail the add");
    let wrong_out = format!("{}{}", stdout_text(&wrong), stderr_text(&wrong));
    assert!(wrong_out.contains("checksum mismatch"), "{wrong_out}");
    assert!(
        wrong_out.contains("spec declaration rolled back"),
        "{wrong_out}"
    );
    assert!(
        !sandbox.spec().contains("target ="),
        "no stranded spec entry: {}",
        sandbox.spec()
    );
    let listed = run_niu_with_env(&["plugin", "source", "list"], &sandbox.envs);
    assert!(
        stdout_text(&listed).contains("no sources installed"),
        "{}",
        stdout_text(&listed)
    );

    // Learn the fixture's real checksum from an unpinned install, remove
    // it, then re-add pinned: the correct checksum installs.
    let plain = run_niu_with_env(
        &["plugin", "add", "oh-my-bash", "--path", &origin_s],
        &sandbox.envs,
    );
    assert_success(&plain, "unpinned add");
    let out = stdout_text(&plain);
    let sha = out
        .split("tree sha256 ")
        .nth(1)
        .map(|rest| rest.split_whitespace().next().unwrap_or("").to_string())
        .unwrap_or_default();
    assert_eq!(sha.len(), 64, "expected a sha256 in the receipt: {out}");

    let remove = run_niu_with_env(&["plugin", "source", "remove", "oh-my-bash"], &sandbox.envs);
    assert_success(&remove, "remove the unpinned install");

    let pinned = run_niu_with_env(
        &[
            "plugin",
            "add",
            "oh-my-bash",
            "--path",
            &origin_s,
            "--checksum",
            &sha,
        ],
        &sandbox.envs,
    );
    assert_success(&pinned, "the correct checksum must install");
    assert!(
        stdout_text(&pinned).contains("Installed source 'oh-my-bash'"),
        "{}",
        stdout_text(&pinned)
    );

    let _ = fs::remove_dir_all(sandbox.home.parent().unwrap());
}

/// A failed `niu plugin add` leaves NOTHING behind (wt83 #174): the spec
/// declaration is rolled back, bootstrap remains a silent no-op, and
/// a legacy stranded entry (declared, never installed) is
/// removed by `niu plugin sync --prune` with a printed line.
#[test]
fn failed_add_rolls_back_and_prune_removes_legacy_strands() {
    fn plugin_state_snapshot(directory: &std::path::Path) -> Vec<(PathBuf, Option<Vec<u8>>)> {
        let mut entries = Vec::new();
        for entry in fs::read_dir(directory).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                entries.push((path.clone(), None));
                entries.extend(plugin_state_snapshot(&path));
            } else {
                entries.push((path.clone(), Some(fs::read(path).unwrap())));
            }
        }
        entries.sort();
        entries
    }
    let sandbox = Sandbox::new("add-rollback");
    // A tree with no manager layout and no sourceable shell file: the
    // fetch fails honestly (the audit's a14 shape, offline).
    let empty = fixture("no-sourceable");

    let bad = run_niu_with_env(
        &[
            "plugin",
            "add",
            "oh-my-bash",
            "--path",
            &empty.to_string_lossy(),
        ],
        &sandbox.envs,
    );
    assert!(!bad.status.success(), "no-asset tree must fail the add");
    let bad_out = format!("{}{}", stdout_text(&bad), stderr_text(&bad));
    assert!(bad_out.contains("could not install"), "{bad_out}");
    assert!(
        bad_out.contains("spec declaration rolled back"),
        "{bad_out}"
    );
    // Spec, registry, and tree: all clean.
    assert!(
        !sandbox.spec().contains("target ="),
        "no stranded spec entry: {}",
        sandbox.spec()
    );
    assert!(
        !sandbox.sources_root.join("registry.toml").exists(),
        "no registry record"
    );
    assert!(!sandbox.sources_root.join("oh-my-bash").exists());

    // The startup form has nothing to defer or nag about.
    let boot = run_niu_with_env(&["plugin", "sync", "--bootstrap"], &sandbox.envs);
    assert_success(&boot, "bootstrap after rolled-back add");
    assert!(
        stdout_text(&boot).is_empty() && stderr_text(&boot).is_empty(),
        "startup must be silent after a rolled-back add: {}",
        stderr_text(&boot)
    );

    // A LEGACY strand (declared by an old add, never installed): the
    // compatibility no-op must leave it alone; explicit --prune removes it.
    let missing = sandbox.home.parent().unwrap().join("missing-tree");
    fs::write(
        sandbox.spec_path(),
        format!(
            "schema = \"niubash:plugin-spec@0.1.0\"\n\n[[sources]]\ntarget = '{}'\nid = 'stranded'\n",
            missing.to_string_lossy().replace('\\', "/")
        ),
    )
    .unwrap();

    let state_before = plugin_state_snapshot(sandbox.home.parent().unwrap());
    for _ in 0..2 {
        let boot = run_niu_with_env(&["plugin", "sync", "--bootstrap"], &sandbox.envs);
        assert_success(&boot, "bootstrap with stranded declaration");
        assert!(stdout_text(&boot).is_empty() && stderr_text(&boot).is_empty());
        assert_eq!(
            plugin_state_snapshot(sandbox.home.parent().unwrap()),
            state_before
        );
    }

    let prune = run_niu_with_env(&["plugin", "sync", "--prune"], &sandbox.envs);
    assert_success(&prune, "prune runs");
    assert!(
        stdout_text(&prune).contains("stranded declaration pruned"),
        "{}",
        stdout_text(&prune)
    );
    assert!(
        !sandbox.spec().contains("missing-tree"),
        "the strand is gone: {}",
        sandbox.spec()
    );
    let boot3 = run_niu_with_env(&["plugin", "sync", "--bootstrap"], &sandbox.envs);
    assert_success(&boot3, "bootstrap after prune");
    assert!(
        stdout_text(&boot3).is_empty() && stderr_text(&boot3).is_empty(),
        "startup is silent again: {}",
        stderr_text(&boot3)
    );

    let _ = fs::remove_dir_all(sandbox.home.parent().unwrap());
}

/// niubash#293 + niubash#295: `niu plugin add` of a dead origin fails
/// BOUNDED at the fetch budget, the error names the origin, and the spec
/// declaration is rolled back — the strand that used to deadlock every
/// later add ("already declared") never lands, and the same target can be
/// added again immediately.
#[test]
fn plugin_add_of_a_dead_origin_times_out_rolls_back_and_retries() {
    let mut sandbox = Sandbox::new("add-dead-origin");
    // Keep the budget tiny: the blackhole (RFC 5737 TEST-NET-3) hangs the
    // connect phase, the budget kills the clone's whole process tree.
    sandbox
        .envs
        .push(("NIU_PLUGIN_FETCH_TIMEOUT_MS", "800".to_string()));

    let first = run_niu_with_env(
        &["plugin", "add", "https://203.0.113.1/stalled-plugin.git"],
        &sandbox.envs,
    );
    assert!(
        !first.status.success(),
        "a dead origin must fail the add: stdout={} stderr={}",
        stdout_text(&first),
        stderr_text(&first)
    );
    let combined = format!("{}{}", stdout_text(&first), stderr_text(&first));
    assert!(
        combined.contains("203.0.113.1"),
        "the failure must name the origin: {combined}"
    );
    assert!(
        combined.contains("rolled back"),
        "the spec declaration must roll back: {combined}"
    );
    assert!(
        !sandbox.spec().contains("stalled-plugin"),
        "no stranded declaration: {}",
        sandbox.spec()
    );
    assert!(
        !sandbox
            .sources_root
            .read_dir()
            .map(|entries| entries
                .filter_map(Result::ok)
                .any(|entry| entry.file_name().to_string_lossy().starts_with(".staging-")))
            .unwrap_or(false),
        "no staging leftovers"
    );

    // The deadlock is broken: the same target adds again and fails with
    // the SAME honest fetch error, never "already declared".
    let second = run_niu_with_env(
        &["plugin", "add", "https://203.0.113.1/stalled-plugin.git"],
        &sandbox.envs,
    );
    assert!(
        !second.status.success(),
        "the retry fetches and fails again"
    );
    let combined = format!("{}{}", stdout_text(&second), stderr_text(&second));
    assert!(
        !combined.contains("already declared"),
        "no stranded-declaration deadlock: {combined}"
    );
    assert!(
        combined.contains("rolled back"),
        "the retried add rolls back too: {combined}"
    );

    let _ = fs::remove_dir_all(sandbox.home.parent().unwrap());
}

/// niubash#293: `niu plugin sync --timeout <seconds>` parses, and a bogus
/// value is refused with a clear message (the flag is the verb-level knob
/// for peculiar networks).
#[test]
fn plugin_sync_timeout_flag_parses_and_validates() {
    let sandbox = Sandbox::new("sync-timeout-flag");

    let bad = run_niu_with_env(&["plugin", "sync", "--timeout", "0"], &sandbox.envs);
    assert!(!bad.status.success(), "zero seconds must be refused");
    assert!(
        stderr_text(&bad).contains("--timeout"),
        "{}",
        stderr_text(&bad)
    );

    let bad = run_niu_with_env(&["plugin", "sync", "--timeout=abc"], &sandbox.envs);
    assert!(!bad.status.success(), "non-numeric must be refused");
    assert!(
        stderr_text(&bad).contains("--timeout"),
        "{}",
        stderr_text(&bad)
    );

    // A healthy value passes parsing and reaches the reconciler (empty
    // spec: legacy-mode report, exit 0).
    let good = run_niu_with_env(&["plugin", "sync", "--timeout", "120"], &sandbox.envs);
    assert_success(&good, "valid --timeout");

    let _ = fs::remove_dir_all(sandbox.home.parent().unwrap());
}
