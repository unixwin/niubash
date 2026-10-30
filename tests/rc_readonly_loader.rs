//! niubash#359 gate cases (design doc `rc-readonly-redesign.md` §5, rubash#746
//! ratchet format): the generated plugin loader, the read-only rc contract,
//! the double-execution guard, `niu plugin migrate-rc`, and the rollback
//! switch — exercised end-to-end against the built `niu` binary.
//!
//! Case map (this file pins the 1.4.2 slice):
//! - 359.4  explicit verbs never write the rc (success AND failure paths)
//! - 359.6  the #324 corrupt rc survives every verb byte-identically
//! - 359.7  loader generation is byte-stable + idempotent (no rewrite when
//!          nothing changed; validate-then-atomic publish)
//! - 359.8  double-execution guard: a legacy managed block keeps the loader
//!          OFF; removing it by hand turns the loader ON (exactly one path)
//! - 359.9  order and override: loader before rc, user overrides win;
//!          the rollback switch (`NIU_PLUGIN_LOADER=off`) skips loading
//!          with zero writes
//! - 359.12 structure ratchet: the retired managed-block writers have no
//!          call sites outside their rollback zone (source-level audit)
//! - migrate-rc: the read-only diagnostic (rc untouched, candidate diff,
//!          loader publish, hint marker)

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::SystemTime;

fn niu_binary() -> PathBuf {
    let p = PathBuf::from(env!("CARGO_BIN_EXE_niu"));
    if p.exists() {
        return p;
    }
    let mut fallback = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    fallback.push("target");
    fallback.push("debug");
    fallback.push(if cfg!(windows) { "niu.exe" } else { "niu" });
    fallback
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
    temp: PathBuf,
    envs: Vec<(&'static str, String)>,
}

impl Sandbox {
    fn new(label: &str) -> Self {
        let nanos = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let temp = std::env::temp_dir().join(format!(
            "niu-rc-readonly-{label}-{}-{nanos}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&temp);
        let home = temp.join("home");
        fs::create_dir_all(&home).unwrap();
        let sources_root = temp.join("sources");
        let envs = vec![
            ("HOME", home.to_string_lossy().into_owned()),
            ("USERPROFILE", home.to_string_lossy().into_owned()),
            (
                "NIU_PLUGIN_SOURCES_ROOT",
                sources_root.to_string_lossy().into_owned(),
            ),
            (
                "NIU_PLUGIN_SPEC",
                home.join(".niubash/plugins.json")
                    .to_string_lossy()
                    .into_owned(),
            ),
        ];
        Sandbox { home, temp, envs }
    }

    fn rc_path(&self) -> PathBuf {
        self.home.join(".niubashrc")
    }

    fn loader_path(&self) -> PathBuf {
        self.home
            .join(".niubash")
            .join("generated")
            .join("plugins.niu")
    }

    fn rc(&self) -> String {
        fs::read_to_string(self.rc_path()).unwrap_or_default()
    }

    fn loader(&self) -> String {
        fs::read_to_string(self.loader_path()).unwrap_or_default()
    }

    /// The rc's bytes + size + mtime, the identity gate 359.4 pins.
    fn rc_identity(&self) -> (Vec<u8>, u64, SystemTime) {
        let meta = fs::metadata(self.rc_path()).unwrap();
        (
            fs::read(self.rc_path()).unwrap(),
            meta.len(),
            meta.modified().unwrap(),
        )
    }

    fn assert_rc_identity_unchanged(&self, before: &(Vec<u8>, u64, SystemTime)) {
        let after = self.rc_identity();
        assert_eq!(before.0, after.0, "rc bytes changed (niubash#359)");
        assert_eq!(before.1, after.1, "rc length changed (niubash#359)");
        assert_eq!(
            before.2, after.2,
            "rc mtime changed — something rewrote the rc (niubash#359)"
        );
    }

    /// Sorted (name, len) inventory of ~/.niubash — the zero-write config
    /// dir check (359.9's opt-out matrix).
    fn config_inventory(&self) -> Vec<(String, u64)> {
        let root = self.home.join(".niubash");
        let mut out = Vec::new();
        fn walk(dir: &Path, prefix: &str, out: &mut Vec<(String, u64)>) {
            let Ok(entries) = fs::read_dir(dir) else {
                return;
            };
            for entry in entries.filter_map(Result::ok) {
                let name = format!(
                    "{prefix}{}",
                    entry.file_name().to_string_lossy().into_owned()
                );
                let path = entry.path();
                if path.is_dir() {
                    walk(&path, &format!("{name}/"), out);
                } else if let Ok(meta) = entry.metadata() {
                    out.push((name, meta.len()));
                }
            }
        }
        walk(&root, "", &mut out);
        out.sort();
        out
    }

    fn run(&self, args: &[&str]) -> Output {
        run_niu(args, &self.envs)
    }

    /// An interactive one-shot: `-i -c` sources the rc branch (loader then
    /// rc) exactly like a REPL startup.
    fn run_interactive(&self, script: &str) -> Output {
        run_niu(&["-i", "-c", script], &self.envs)
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.temp);
    }
}

fn run_niu(args: &[&str], envs: &[(&'static str, String)]) -> Output {
    let mut command = Command::new(niu_binary());
    command.args(args);
    for (key, value) in envs {
        command.env(key, value);
    }
    command.env_remove("NIU_PLUGIN_LOADER");
    command
        .output()
        .unwrap_or_else(|err| panic!("failed to run niu {args:?}: {err}"))
}

fn stdout_text(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr_text(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn assert_success(output: &Output, context: &str) {
    assert!(
        output.status.success(),
        "{context} failed with {}:\nstdout:\n{}\nstderr:\n{}",
        output.status,
        stdout_text(output),
        stderr_text(output)
    );
}

/// Install the local oh-my-bash fixture (offline).
fn install_omb(sandbox: &Sandbox) {
    assert_success(
        &sandbox.run(&[
            "plugin",
            "add",
            "oh-my-bash",
            "--path",
            &fixture("oh-my-bash").to_string_lossy(),
        ]),
        "plugin add oh-my-bash --path",
    );
}

/// The write-era managed block for the fixture (what an old niu left in a
/// pre-migration rc).
fn legacy_omb_block() -> String {
    format!(
        "# >>> niu source oh-my-bash (managed by `niu plugin enable/disable`) >>>\n\
         OSH_THEME='robbyrussell'\n\
         export NIU_RC_MARK=rcblock\n\
         # <<< niu source oh-my-bash <<<\n"
    )
}

/// A minimal valid loader document with one hand-written section.
fn loader_doc_with(body: &str) -> String {
    format!(
        "# niubash generated plugin loader — DO NOT EDIT (regenerated by `niu plugin` verbs).\n\
         # schema: niubash:plugin-loader@1\n\
         # generated from ~/.niubash/plugins.json + ~/.niubash/sources/registry.json\n\
         # sourced by the host BEFORE the user rc; user overrides win.\n\
         # superseded managed-block markers: (none recorded)\n\
         \n\
         # --- niu plugin source: testprobe ---\n\
         {body}\n\
         # --- end niu plugin source: testprobe ---\n"
    )
}

/// ── 359.4 ──────────────────────────────────────────────────────────────────
///
/// Every explicit management verb leaves the rc byte- and mtime-identical,
/// on success and failure paths alike (design §2.2: the verbs keep their
/// write rights over the sidecar manifest only).
#[test]
fn case_359_4_explicit_verbs_never_write_the_rc() {
    let sandbox = Sandbox::new("359-4");
    fs::write(
        sandbox.rc_path(),
        "# my rc\nexport MY_VAR=1\nalias ll='ls -la'\n",
    )
    .unwrap();
    install_omb(&sandbox);
    let identity = sandbox.rc_identity();

    // Success paths.
    for verb in [
        vec!["plugin", "enable", "git"],
        vec!["plugin", "enable", "agnoster"],
        vec!["plugin", "sync"],
        vec!["plugin", "disable", "agnoster"],
        vec!["plugin", "disable", "git"],
        vec!["plugin", "enable", "oh-my-bash"],
        vec!["plugin", "disable", "oh-my-bash"],
    ] {
        let out = sandbox.run(&verb);
        assert_success(&out, &format!("verb {verb:?}"));
        sandbox.assert_rc_identity_unchanged(&identity);
    }

    // Outcome-agnostic paths (a --path snapshot has no upstream, so
    // restore/update may refuse): whatever the verb decides, the rc is
    // never part of it.
    for verb in [
        vec!["plugin", "restore", "oh-my-bash"],
        vec!["plugin", "update", "oh-my-bash"],
    ] {
        let _ = sandbox.run(&verb);
        sandbox.assert_rc_identity_unchanged(&identity);
    }

    // Failure paths refuse identically: unknown targets exit nonzero
    // without touching the rc.
    for verb in [
        vec!["plugin", "enable", "no-such-asset"],
        vec!["plugin", "enable", "nope/nope"],
    ] {
        let out = sandbox.run(&verb);
        assert!(
            !out.status.success(),
            "verb {verb:?} must fail: {}",
            stdout_text(&out)
        );
        sandbox.assert_rc_identity_unchanged(&identity);
    }

    // The activation plane moved: a fresh enable lands in the loader (the
    // rc never sees a managed block).
    assert_success(&sandbox.run(&["plugin", "enable", "git"]), "final enable");
    assert!(
        sandbox.loader().contains("plugins=('git')"),
        "{}",
        sandbox.loader()
    );
    assert!(
        !sandbox.rc().contains("niu source"),
        "no managed block in the rc: {}",
        sandbox.rc()
    );
}

/// ── 359.6 ──────────────────────────────────────────────────────────────────
///
/// The #324 corrupt fixture (six moved-out guard sections, five missing
/// their `fi`) survives every verb byte-identically: niu never "repairs"
/// the rc anymore — the evidence belongs to the user (doctor/migrate-rc
/// report; the fix is the user's edit).
#[test]
fn case_359_6_corrupt_rc_is_preserved_through_all_verbs() {
    let sandbox = Sandbox::new("359-6");
    install_omb(&sandbox);
    // Canonical loader generation first (as a post-migration machine has).
    assert_success(&sandbox.run(&["plugin", "enable", "git"]), "enable");

    let guard = |with_fi: bool| {
        format!(
            "# drift guard (static): `niu plugin sync` lists drifted ids in drifted-sources.txt\n\
             _niu_drift=\"${{NIU_PLUGIN_SOURCES_ROOT:-$HOME/.niubash/sources}}/drifted-sources.txt\"\n\
             if [ -s \"$_niu_drift\" ] && grep -qx 'oh-my-bash' \"$_niu_drift\" 2>/dev/null; then\n\
             \x20 echo drifted >&2\n\
             {}unset _niu_drift\n",
            if with_fi { "fi\n" } else { "" }
        )
    };
    let mut corrupt = legacy_omb_block();
    for with_fi in [false, false, false, false, false, true] {
        corrupt.push_str("# user lines moved out of a managed block by `niu plugin` (niu #176)\n");
        corrupt.push_str(&guard(with_fi));
    }
    fs::write(sandbox.rc_path(), &corrupt).unwrap();
    let corrupt_bytes = fs::read(sandbox.rc_path()).unwrap();

    for verb in [
        vec!["plugin", "sync"],
        vec!["plugin", "enable", "cargo"],
        vec!["plugin", "disable", "cargo"],
        vec!["plugin", "enable", "oh-my-bash"],
        vec!["plugin", "disable", "oh-my-bash"],
        vec!["plugin", "migrate-rc"],
        vec!["doctor"],
    ] {
        let out = sandbox.run(&verb);
        assert_success(&out, &format!("corrupt-rc verb {verb:?}"));
        assert_eq!(
            fs::read(sandbox.rc_path()).unwrap(),
            corrupt_bytes,
            "verb {verb:?} rewrote the corrupt rc (niubash#359 forbids rc writes)"
        );
    }
}

/// ── 359.7 (binary half) ────────────────────────────────────────────────────
///
/// Regeneration is idempotent and byte-stable: an unchanged manifest
/// re-renders the exact same bytes and does NOT rewrite the file (mtime
/// untouched), so the loader is a build artifact, not a churn surface.
#[test]
fn case_359_7_loader_regeneration_is_byte_stable_and_idempotent() {
    let sandbox = Sandbox::new("359-7");
    install_omb(&sandbox);
    assert_success(&sandbox.run(&["plugin", "enable", "git"]), "enable 1");
    assert_success(&sandbox.run(&["plugin", "enable", "agnoster"]), "enable 2");
    let first = fs::read(sandbox.loader_path()).unwrap();
    let first_meta = fs::metadata(sandbox.loader_path()).unwrap();
    let first_mtime = first_meta.modified().unwrap();

    // A plain sync changes nothing: byte-identical, mtime identical.
    assert_success(&sandbox.run(&["plugin", "sync"]), "sync");
    assert_eq!(fs::read(sandbox.loader_path()).unwrap(), first);
    assert_eq!(
        fs::metadata(sandbox.loader_path())
            .unwrap()
            .modified()
            .unwrap(),
        first_mtime,
        "an unchanged manifest must not rewrite the generation"
    );

    // A disable changes the generation through one atomic swap; the file is
    // never absent for a reader that opened it before.
    let reader_handle = fs::File::open(sandbox.loader_path()).unwrap();
    assert_success(&sandbox.run(&["plugin", "disable", "agnoster"]), "disable");
    let second = fs::read(sandbox.loader_path()).unwrap();
    assert_ne!(first, second, "the generation changed");
    drop(reader_handle);
    assert!(
        String::from_utf8_lossy(&second).contains("# --- niu plugin source: oh-my-bash ---"),
        "the remaining section is intact: {}",
        String::from_utf8_lossy(&second)
    );
}

/// ── 359.8 ──────────────────────────────────────────────────────────────────
///
/// No double execution: while the user rc carries a superseded managed
/// block, the host loader stays OFF (the old block is the live path); once
/// the user removes the block BY HAND, the loader takes over. Exactly one
/// activation path at any time, plus the one-time migration hint.
#[test]
fn case_359_8_double_execution_guard_and_hand_removal() {
    let sandbox = Sandbox::new("359-8");
    install_omb(&sandbox);
    assert_success(&sandbox.run(&["plugin", "enable", "git"]), "enable");

    // The loader generation sets its own mark; the legacy rc block sets
    // another. While both exist, only the rc block may run.
    let loader = sandbox.loader();
    let marked = loader.replace(
        "# --- niu plugin source: oh-my-bash ---",
        "# --- niu plugin source: oh-my-bash ---\nexport NIU_LOADER_MARK=loader",
    );
    fs::write(sandbox.loader_path(), marked).unwrap();
    fs::write(sandbox.rc_path(), legacy_omb_block()).unwrap();

    let probe = r#"printf '%s' "${NIU_LOADER_MARK-}|${NIU_RC_MARK-}""#;
    let out = sandbox.run_interactive(probe);
    assert_success(&out, "interactive probe with legacy block");
    assert_eq!(
        stdout_text(&out).trim_end(),
        "|rcblock",
        "the loader must stay OFF while the legacy block executes: {}",
        stdout_text(&out)
    );
    // The one-time migration hint fired...
    assert!(
        stderr_text(&out).contains("migrate-rc"),
        "the transition must be announced: {}",
        stderr_text(&out)
    );
    let hint_marker = sandbox
        .home
        .join(".niubash")
        .join("generated")
        .join(".legacy-block-hint-shown");
    assert!(hint_marker.is_file(), "the hint marker records the nudge");
    // ...exactly once.
    let out2 = sandbox.run_interactive(probe);
    assert_success(&out2, "second probe");
    assert!(
        !stderr_text(&out2).contains("migrate-rc"),
        "the hint must not repeat: {}",
        stderr_text(&out2)
    );

    // The user removes the block by hand (the only removal niu ever offers):
    // the loader becomes the single live path.
    fs::write(
        sandbox.rc_path(),
        "# my rc, block removed\nexport MY_OWN=1\n",
    )
    .unwrap();
    let out3 = sandbox.run_interactive(probe);
    assert_success(&out3, "probe after hand removal");
    assert_eq!(
        stdout_text(&out3).trim_end(),
        "loader|",
        "the loader takes over after the hand removal: {}",
        stdout_text(&out3)
    );
    assert!(
        !stderr_text(&out3).contains("migrate-rc"),
        "no hint for a clean rc: {}",
        stderr_text(&out3)
    );
}

/// ── 359.9 ──────────────────────────────────────────────────────────────────
///
/// Order and override: the loader sources BEFORE the user rc, so user
/// PS1/alias/variable overrides win; the rollback switch
/// (`NIU_PLUGIN_LOADER=off`) disables loading with ZERO writes anywhere.
#[test]
fn case_359_9_order_override_and_rollback_switch() {
    let sandbox = Sandbox::new("359-9");
    fs::create_dir_all(sandbox.loader_path().parent().unwrap()).unwrap();
    fs::write(
        sandbox.loader_path(),
        loader_doc_with(
            "export NIU_PLUGIN_FLOOR=floor\nexport PS1='plugin> '\nexport NIU_LOADER_LOADS=yes",
        ),
    )
    .unwrap();
    fs::write(
        sandbox.rc_path(),
        "export NIU_USER_TOP=user\nexport PS1='user> '\n",
    )
    .unwrap();
    let inventory_before = sandbox.config_inventory();

    // Default: both load, loader first, user wins.
    let probe = r#"printf '%s|%s|%s' "$NIU_PLUGIN_FLOOR" "$NIU_USER_TOP" "$PS1""#;
    let out = sandbox.run_interactive(probe);
    assert_success(&out, "interactive override probe");
    assert_eq!(
        stdout_text(&out).trim_end(),
        "floor|user|user>",
        "the loader runs before the rc; the user override wins: {}",
        stdout_text(&out)
    );
    assert_eq!(
        sandbox.config_inventory(),
        inventory_before,
        "a clean startup writes nothing into ~/.niubash"
    );

    // Rollback switch: the loader is skipped, the rc still runs, and
    // nothing anywhere is written.
    let mut command = Command::new(niu_binary());
    command.args(["-i", "-c", probe]);
    for (key, value) in &sandbox.envs {
        command.env(key, value);
    }
    command.env("NIU_PLUGIN_LOADER", "off");
    let rolled = command.output().unwrap();
    assert_success(&rolled, "rolled-back probe");
    assert_eq!(
        stdout_text(&rolled).trim_end(),
        "|user|user>",
        "the switch stops the loader and nothing else: {}",
        stdout_text(&rolled)
    );
    assert_eq!(
        sandbox.config_inventory(),
        inventory_before,
        "the rollback switch must not write anything"
    );
    // Unset (roll forward): the loader loads again — the switch is
    // load-only, it never deleted or regenerated anything.
    let out2 = sandbox.run_interactive(probe);
    assert_success(&out2, "roll-forward probe");
    assert_eq!(stdout_text(&out2).trim_end(), "floor|user|user>");
}

/// ── migrate-rc ─────────────────────────────────────────────────────────────
///
/// The read-only migration diagnostic: publishes the candidate generation,
/// reports every legacy block with its line range and ownership, writes the
/// suggested post-migration rc ONLY under ~/.niubash/candidates/ with
/// --diff, and never modifies the rc.
#[test]
fn migrate_rc_reports_never_writes_and_candidates_the_diff() {
    let sandbox = Sandbox::new("migrate-rc");
    install_omb(&sandbox);
    assert_success(&sandbox.run(&["plugin", "enable", "git"]), "enable");
    // The user's rc with a legacy block plus real user lines around it.
    fs::write(
        sandbox.rc_path(),
        format!(
            "# my rc\nexport MY_VAR=1\n{}export AFTER=1\n",
            legacy_omb_block()
        ),
    )
    .unwrap();
    let identity = sandbox.rc_identity();

    let out = sandbox.run(&["plugin", "migrate-rc", "--diff"]);
    assert_success(&out, "migrate-rc --diff");
    sandbox.assert_rc_identity_unchanged(&identity);

    let report = stdout_text(&out);
    assert!(report.contains("lines"), "{report}");
    assert!(report.contains("oh-my-bash"), "{report}");
    assert!(report.contains("declared"), "{report}");
    assert!(report.contains("candidates"), "{report}");

    // The candidate file: under candidates/, holding the user's lines and
    // NONE of the managed block.
    let candidate = sandbox
        .home
        .join(".niubash")
        .join("candidates")
        .join("rc-after-plugin-migration.niu");
    assert!(candidate.is_file(), "the candidate diff was written");
    let suggested = fs::read_to_string(&candidate).unwrap();
    assert!(suggested.contains("MY_VAR=1"), "{suggested}");
    assert!(suggested.contains("AFTER=1"), "{suggested}");
    assert!(
        !suggested.contains("niu source"),
        "the suggested rc has no managed block: {suggested}"
    );

    // The loader generation was published (with the superseded record).
    let loader = sandbox.loader();
    assert!(
        loader.contains("plugins=('git')"),
        "the manifest's selection is in the generation: {loader}"
    );
    assert!(
        loader.contains("superseded-marker: # >>> niu source oh-my-bash"),
        "the superseded pair is recorded: {loader}"
    );

    // A second run is idempotent and reports the same blocks (the user has
    // not removed them yet — niu still does not).
    let again = sandbox.run(&["plugin", "migrate-rc"]);
    assert_success(&again, "migrate-rc again");
    sandbox.assert_rc_identity_unchanged(&identity);
    let again_report = stdout_text(&again);
    assert!(
        again_report.contains("oh-my-bash"),
        "the second report still names the block: {again_report}"
    );
}

/// migrate-rc on a clean machine: an honest "clean" report, the rc (absent)
/// untouched, and no candidate file invented.
#[test]
fn migrate_rc_on_a_clean_machine_is_a_honest_noop() {
    let sandbox = Sandbox::new("migrate-clean");
    let out = sandbox.run(&["plugin", "migrate-rc", "--diff"]);
    assert_success(&out, "migrate-rc on empty home");
    let report = stdout_text(&out);
    assert!(report.contains("clean"), "{report}");
    assert!(!sandbox.rc_path().exists(), "the rc must not be created");
    assert!(
        !sandbox.home.join(".niubash/candidates").exists(),
        "no candidate for a clean rc"
    );
}

/// CR F2, end to end: when the guard trips on lines the id-anchored scan
/// cannot attribute (an id-less marker pair), migrate-rc must name the trip
/// lines — NEVER print "clean" over a held-off loader. The rc stays
/// untouched, as everywhere else in this file.
#[test]
fn migrate_rc_names_guard_trips_the_scan_cannot_attribute() {
    let sandbox = Sandbox::new("guard-trip-report");
    install_omb(&sandbox);
    assert_success(&sandbox.run(&["plugin", "enable", "git"]), "enable");
    // An id-less marker pair wrapping real content: the guard trips, the
    // id-anchored scan finds no id to attribute it to.
    fs::write(
        sandbox.rc_path(),
        "# my rc\n# >>> niu source >>>\nexport LEFTOVER=1\n# <<< niu source <<<\n",
    )
    .unwrap();
    let identity = sandbox.rc_identity();

    let out = sandbox.run(&["plugin", "migrate-rc"]);
    assert_success(&out, "migrate-rc with an id-less pair");
    sandbox.assert_rc_identity_unchanged(&identity);
    let report = stdout_text(&out);
    assert!(
        report.contains("guard:"),
        "the report must name the guard trip: {report}"
    );
    assert!(
        report.contains("LEFTOVER") || report.contains("line"),
        "the trip lines must be visible: {report}"
    );
    assert!(
        !report.contains("clean:"),
        "a held-off loader must never read as clean: {report}"
    );

    // Removing the stray pair by hand releases the loader: clean verdict.
    fs::write(sandbox.rc_path(), "# my rc\nexport MY_OWN=1\n").unwrap();
    let out2 = sandbox.run(&["plugin", "migrate-rc"]);
    assert_success(&out2, "migrate-rc on a clean rc");
    assert!(
        stdout_text(&out2).contains("clean:"),
        "a clean rc reads as clean: {}",
        stdout_text(&out2)
    );
}

/// The doctor names the transition state read-only (advisory row, no
/// repair, no rc write).
#[test]
fn doctor_reports_legacy_blocks_read_only() {
    let sandbox = Sandbox::new("doctor-legacy");
    fs::write(sandbox.rc_path(), legacy_omb_block()).unwrap();
    let identity = sandbox.rc_identity();
    let out = sandbox.run(&["doctor"]);
    assert_success(&out, "doctor");
    sandbox.assert_rc_identity_unchanged(&identity);
    let report = stdout_text(&out);
    assert!(
        report.contains("legacy rc blocks"),
        "the doctor must name the legacy blocks: {report}"
    );
    assert!(
        report.contains("migrate-rc"),
        "the doctor must point at the migration verb: {report}"
    );
}

/// ── 359.12 ─────────────────────────────────────────────────────────────────
///
/// The structure ratchet: the retired managed-block writers remain in the
/// tree (the code-level rollback contract, design §4.3) but are reachable
/// from NO verb path — any new call site makes this fail, which is the
/// ratchet tooth against #324-style regressions re-attaching.
/// Strip `#[cfg(test)] mod ...` blocks (brace-matched) so the ratchet
/// counts only production code — the in-crate tests may exercise the
/// rollback-zone writers directly, that is what they are FOR.
fn strip_test_modules(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(pos) = rest.find("#[cfg(test)]") {
        let Some(mod_rel) = rest[pos..].find("\nmod ") else {
            break;
        };
        let mod_pos = pos + mod_rel;
        let Some(brace_rel) = rest[mod_pos..].find('{') else {
            break;
        };
        let brace = mod_pos + brace_rel;
        out.push_str(&rest[..pos]);
        let bytes = rest.as_bytes();
        let mut depth = 0usize;
        let mut i = brace;
        while i < bytes.len() {
            match bytes[i] {
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
                _ => {}
            }
            i += 1;
        }
        rest = &rest[i + 1..];
    }
    out.push_str(rest);
    out
}

#[test]
fn case_359_12_retired_writers_have_no_verb_call_sites() {
    // CR F1: the audit walks the ENTIRE production source tree (both
    // crates' src/ and the binary's src/), not a hand-maintained file
    // list — a new module, or a call smuggled into setup_wizard.rs /
    // repl.rs, is scanned the same as everything else. The
    // mutation this closes: `assets::deactivate_block` used to be `pub`,
    // so any module could compile-passing reach `remove_managed_block`
    // through it; it is now private, and the tree-wide absence scan makes
    // even a private re-wiring visible.
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut sources: Vec<PathBuf> = Vec::new();
    fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries.filter_map(Result::ok) {
            let path = entry.path();
            if path.is_dir() {
                collect(&path, out);
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                out.push(path);
            }
        }
    }
    collect(&manifest.join("crates/niubash-runtime/src"), &mut sources);
    collect(&manifest.join("src"), &mut sources);
    assert!(
        sources.len() >= 40,
        "the source walk found only {} files — the tree layout moved; fix the roots",
        sources.len()
    );

    const RETIRED: [&str; 5] = [
        "write_managed_block(",
        "remove_managed_block(",
        "apply_block(",
        "write_block_report(",
        "deactivate_block(",
    ];
    // The rollback-zone internal wiring allowed in assets.rs (each
    // symbol's definition plus only the zone-internal calls between the
    // retired helpers themselves).
    const ZONE_ALLOWED: [usize; 5] = [2, 3, 1, 2, 1];

    let assets_path = manifest.join("crates/niubash-runtime/src/plugins/assets.rs");
    for path in &sources {
        let raw = fs::read_to_string(path).unwrap();
        let display = path.strip_prefix(&manifest).unwrap_or(path).display();
        if path == &assets_path {
            // The zone itself: the writers EXIST (small-diff rollback
            // contract) and are referenced ONLY by the zone-internal
            // shape above, with in-crate tests stripped.
            let production = strip_test_modules(&raw);
            for (symbol, allowed) in RETIRED.iter().zip(ZONE_ALLOWED.iter()) {
                assert!(
                    production.contains(&format!("fn {symbol}")),
                    "rollback zone violated: fn {symbol} was deleted (design §4.3 keeps it)"
                );
                let count = production.matches(symbol).count();
                assert_eq!(
                    count, *allowed,
                    "{symbol} appears {count} times in assets.rs (allowed: {allowed}) — a \
                     call site re-attached the retired rc writer (niubash#359 ratchet)"
                );
            }
        } else {
            // EVERY other production file — including its #[cfg(test)]
            // modules: the integration surface must not exercise the
            // retired path either. Zero tolerance, no closed list.
            for symbol in RETIRED {
                assert!(
                    !raw.contains(symbol),
                    "{} references {symbol} — the retired rc writer leaked out of the \
                     rollback zone (niubash#359 ratchet)",
                    display
                );
            }
        }
    }

    // The setup wizard's whole-file writer stays (1.4.3 converges it to
    // create-only), but it must stay confined to setup_wizard.rs — same
    // tree-wide form as the managed-block ratchet.
    const WIZARD_WRITERS: [&str; 2] = ["write_rc_and_mark_done(", "write_primary_rc("];
    for path in &sources {
        let raw = fs::read_to_string(path).unwrap();
        let is_wizard = path.ends_with("setup_wizard.rs");
        for symbol in WIZARD_WRITERS {
            if is_wizard {
                assert!(
                    raw.contains(&format!("fn {symbol}")),
                    "the wizard rc writer {symbol} vanished (W6 is 1.4.3 scope)"
                );
            } else {
                assert!(
                    !raw.contains(symbol),
                    "{} reaches the wizard's rc writer {symbol} (confine W6 to \
                     setup_wizard.rs, niubash#359 ratchet)",
                    path.display()
                );
            }
        }
    }
}

/// CR F2-prime, side A (fail-safe): an unpaired begin marker — the write
/// era's `end_missing` state — with executable content below it holds the
/// loader OFF. Both markers are bash comments; the body below executes
/// unconditionally, so the pairing requirement must not open a
/// double-execution hole. The loader comes back only after the stray
/// marker is gone.
#[test]
fn lone_begin_with_executable_content_holds_the_loader_off() {
    let sandbox = Sandbox::new("f2p-side-a");
    install_omb(&sandbox);
    assert_success(&sandbox.run(&["plugin", "enable", "git"]), "enable");
    let loader = sandbox.loader();
    let marked = loader.replace(
        "# --- niu plugin source: oh-my-bash ---",
        "# --- niu plugin source: oh-my-bash ---\nexport NIU_LOADER_MARK=loader",
    );
    fs::write(sandbox.loader_path(), marked).unwrap();

    // The end_missing state: begin marker, orphan body, NO end marker.
    fs::write(
        sandbox.rc_path(),
        "# >>> niu source oh-my-bash >>>\nexport NIU_RC_MARK=rcblock\n",
    )
    .unwrap();

    let probe = r#"printf '%s' "${NIU_LOADER_MARK-}|${NIU_RC_MARK-}""#;
    let out = sandbox.run_interactive(probe);
    assert_success(&out, "probe with an unpaired begin");
    assert_eq!(
        stdout_text(&out).trim_end(),
        "|rcblock",
        "an unpaired begin with executable content must hold the loader OFF \
         (no double execution): {}",
        stdout_text(&out)
    );
    // Both surfaces agree it is held off and name the line.
    let migrate = sandbox.run(&["plugin", "migrate-rc"]);
    assert_success(&migrate, "migrate-rc");
    let report = stdout_text(&migrate);
    assert!(
        !report.contains("clean:"),
        "an end_missing rc must not read as clean: {report}"
    );
    let doctor = sandbox.run(&["doctor"]);
    assert_success(&doctor, "doctor");
    let doctor_report = stdout_text(&doctor);
    assert!(
        doctor_report.contains("stays OFF"),
        "the doctor must report the held-off state: {doctor_report}"
    );

    // Deleting the stray marker by hand releases the loader — exactly one
    // live path again. (Re-mark the generation: the migrate-rc run above
    // republished it, which is exactly the regeneration behavior under
    // test.)
    let regenerated = sandbox.loader();
    let remarked = regenerated.replace(
        "# --- niu plugin source: oh-my-bash ---",
        "# --- niu plugin source: oh-my-bash ---\nexport NIU_LOADER_MARK=loader",
    );
    fs::write(sandbox.loader_path(), remarked).unwrap();
    fs::write(sandbox.rc_path(), "# my rc\nexport MY_OWN=1\n").unwrap();
    let out2 = sandbox.run_interactive(probe);
    assert_success(&out2, "probe after hand removal");
    assert_eq!(
        stdout_text(&out2).trim_end(),
        "loader|",
        "the loader takes over after the removal: {}",
        stdout_text(&out2)
    );
}

/// CR F2-prime, side B (inert stays inert): a lone marker followed only by
/// comments and blanks executes nothing — the loader must keep loading,
/// and migrate-rc reads the rc as clean. This is the direction CASE-1
/// (the marker-shaped remark) already pinned; the unpaired-begin rule
/// must not re-tighten it.
#[test]
fn lone_begin_with_only_comments_keeps_the_loader_on() {
    let sandbox = Sandbox::new("f2p-side-b");
    install_omb(&sandbox);
    assert_success(&sandbox.run(&["plugin", "enable", "git"]), "enable");
    let loader = sandbox.loader();
    let marked = loader.replace(
        "# --- niu plugin source: oh-my-bash ---",
        "# --- niu plugin source: oh-my-bash ---\nexport NIU_LOADER_MARK=loader",
    );
    fs::write(sandbox.loader_path(), marked).unwrap();

    fs::write(
        sandbox.rc_path(),
        "# >>> niu source >>>\n# historical note: blocks were removed by hand\n\n",
    )
    .unwrap();

    let probe = r#"printf '%s' "${NIU_LOADER_MARK-}|${NIU_RC_MARK-}""#;
    let out = sandbox.run_interactive(probe);
    assert_success(&out, "probe with an inert lone marker");
    assert_eq!(
        stdout_text(&out).trim_end(),
        "loader|",
        "a comment-only stray marker must NOT hold the loader off: {}",
        stdout_text(&out)
    );
    let migrate = sandbox.run(&["plugin", "migrate-rc"]);
    assert_success(&migrate, "migrate-rc on an inert rc");
    assert!(
        stdout_text(&migrate).contains("clean:"),
        "an inert rc reads as clean: {}",
        stdout_text(&migrate)
    );
}

/// Startup with a legacy-block rc keeps the config dir write to the
/// documented hint marker ONLY (the R1-class exception), and the marker
/// makes the hint one-shot.
#[test]
fn startup_hint_marker_is_the_only_config_dir_write() {
    let sandbox = Sandbox::new("hint-marker");
    install_omb(&sandbox);
    assert_success(&sandbox.run(&["plugin", "enable", "git"]), "enable");
    fs::write(sandbox.rc_path(), legacy_omb_block()).unwrap();
    let before = sandbox.config_inventory();

    let out = sandbox.run_interactive("true");
    assert_success(&out, "first startup with legacy block");
    let after = sandbox.config_inventory();
    let added: Vec<_> = after
        .iter()
        .filter(|entry| !before.contains(entry))
        .collect();
    assert_eq!(
        added.len(),
        1,
        "exactly one new file (the hint marker): before={before:?} after={after:?}"
    );
    assert!(
        added[0].0.ends_with(".legacy-block-hint-shown"),
        "{added:?}"
    );

    // No rc metadata change beyond the marker.
    let meta = fs::metadata(sandbox.rc_path()).unwrap();
    let len = meta.len();
    let out2 = sandbox.run_interactive("true");
    assert_success(&out2, "second startup");
    assert_eq!(fs::metadata(sandbox.rc_path()).unwrap().len(), len);
    assert!(
        !stderr_text(&out2).contains("migrate-rc"),
        "hint shown once"
    );
}

/// niubash#422 hardening: the rbash × generated-loader combo. The loader
/// step lives on the non-login rc branch of run_startup_files and the
/// rbash face arms its restrictions at the finish_startup tail — AFTER
/// the loader and the user rc have sourced — so an rbash-named session
/// loads the generated plugin set, keeps the user override, starts
/// zero-write, and only then runs restricted (PATH/SHELL/ENV/BASH_ENV/
/// HISTFILE readonly, GNU shell.c maybe_make_restricted). Safe by
/// construction; pinned here so the ordering cannot silently move.
#[test]
fn rbash_named_session_loads_the_generated_loader_then_restricts() {
    let sandbox = Sandbox::new("rbash-loader");
    fs::create_dir_all(sandbox.loader_path().parent().unwrap()).unwrap();
    // HISTFILE FIRST: a readonly failure would abort the loader body
    // before the marker, so "loaded" proves the loader ran while the
    // restricted read-only gates were still off (they arm later, at the
    // finish_startup tail).
    fs::write(
        sandbox.loader_path(),
        loader_doc_with("HISTFILE=\"$HOME/loader-history\"\nexport NIU_LOADER_MARK=loaded"),
    )
    .unwrap();
    // Same ordering probe for the rc: an ENV assignment only survives if
    // the rc ran before the restrictions armed.
    fs::write(
        sandbox.rc_path(),
        "ENV=\"${ENV-}rc\"\nexport NIU_RC_MARK=rc\n",
    )
    .unwrap();
    let inventory = sandbox.config_inventory();
    let rc_before = sandbox.rc_identity();
    let loader_before = fs::read(sandbox.loader_path()).unwrap();

    // The rbash face through the shim-forwarded invocation name, on the
    // interactive dispatch whose startup takes the rc branch.
    let probe = concat!(
        r#"printf '%s|%s|' "${NIU_LOADER_MARK-}" "${NIU_RC_MARK-}"; "#,
        r#"case $- in *r*) echo R-YES;; *) echo R-NO;; esac; "#,
        r#"( PATH=/x ) 2>/dev/null; echo P=$?; "#,
        r#"( SHELL=/x ) 2>/dev/null; echo S=$?; "#,
        r#"( ENV=/x ) 2>/dev/null; echo E=$?; "#,
        r#"( BASH_ENV=/x ) 2>/dev/null; echo B=$?; "#,
        r#"( HISTFILE=/x ) 2>/dev/null; echo H=$?"#,
    );
    let mut command = Command::new(niu_binary());
    command.args(["-i", "-c", probe]);
    for (key, value) in &sandbox.envs {
        command.env(key, value);
    }
    command.env("NIU_INVOKED_AS", "rbash");
    command.env_remove("NIU_PLUGIN_LOADER");
    let out = command.output().unwrap();
    assert_success(&out, "rbash loader probe");

    let stdout = stdout_text(&out);
    for expected in [
        "loaded|rc|", // loader before rc, both sourced
        "R-YES",      // the body runs restricted (armed after startup)
        "P=1",
        "S=1",
        "E=1",
        "B=1",
        "H=1", // the five readonly gates
    ] {
        assert!(
            stdout.contains(expected),
            "missing {expected:?} in rbash probe stdout:\n{stdout}\nstderr:\n{}",
            stderr_text(&out)
        );
    }
    assert!(
        !stdout.contains("R-NO"),
        "the body must not run unrestricted:\n{stdout}"
    );

    // Startup stays zero-write: no config-dir churn, rc untouched, and the
    // loader document itself never rewritten.
    assert_eq!(
        sandbox.config_inventory(),
        inventory,
        "a restricted startup writes nothing into ~/.niubash"
    );
    sandbox.assert_rc_identity_unchanged(&rc_before);
    assert_eq!(
        fs::read(sandbox.loader_path()).unwrap(),
        loader_before,
        "loader document is never rewritten by startup"
    );
}
