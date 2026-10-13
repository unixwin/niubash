//! 1.3.0 release smoke suite (wt49/smokesweep; section B reworked for the
//! download retraction 2026-10-04) — one binary-level pass over every leg
//! the release claims, in the order of the release checklist:
//!
//! * A. install chain — `niu plugin add --path` → trust → enable → the
//!   managed rc block actually takes effect in a fresh `niu -c` shell
//!   (oh-my-bash `OSH_THEME`, bash-it theme PS1), and the declarative spec
//!   round-trips through `niu plugin sync` byte-idempotently;
//! * B. download retraction — the compiled-in recipe index is non-empty,
//!   executable-tool recipes recommend package managers instead of
//!   fetching (fully offline), the retired `plugin tool` verb fails with
//!   the retirement message, and `niu font` runs offline as
//!   detection+recommendation;
//! * C. defaults-as-floor — an enabled external theme claims the prompt
//!   slot end-to-end, disabling it releases the slot back to the shell
//!   floor (the product floor's own restore is pinned by the runtime unit
//!   tests referenced in tests/defaults_floor.rs);
//! * D. setup wizard — `niu setup --preset recommended` in an isolated
//!   HOME writes an rc that parses clean (`niu -n`) and sells no retired
//!   stack fields;
//! * E. basics — `niu --version`, `echo`, `seq 1 3 | wc -l` = 3, `cat -n`.
//!
//! Every subprocess this file spawns is timeout-guarded: a hung niu is
//! killed at its deadline and reported as a failure of that leg, so the
//! suite is safe in CI without external watchdogs. No leg touches the
//! network — the shell carries zero download responsibility.

use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Default per-invocation deadline.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(60);

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

fn temp_dir(name: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "niu-smoke130-{name}-{}-{nanos}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// HOME + USERPROFILE + plugin roots pointed at one sandbox: no leg in this
/// file may touch the real user home, sources, or spec.
struct Sandbox {
    home: PathBuf,
    sources_root: PathBuf,
    spec_path: PathBuf,
    envs: Vec<(&'static str, String)>,
    _root: PathBuf,
}

impl Sandbox {
    fn new(label: &str) -> Self {
        let root = temp_dir(label);
        let home = root.join("home");
        let sources_root = root.join("sources");
        fs::create_dir_all(home.join(".niubash")).unwrap();
        fs::create_dir_all(&sources_root).unwrap();
        let spec_path = home.join(".niubash").join("plugins.toml");
        let envs = vec![
            ("HOME", home.to_string_lossy().into_owned()),
            ("USERPROFILE", home.to_string_lossy().into_owned()),
            (
                "NIU_PLUGIN_SOURCES_ROOT",
                sources_root.to_string_lossy().into_owned(),
            ),
            ("NIU_PLUGIN_SPEC", spec_path.to_string_lossy().into_owned()),
            // No pre-existing rc may leak into the boot legs.
            ("NIU_ENV", String::new()),
        ];
        Self {
            home,
            sources_root,
            spec_path,
            envs,
            _root: root,
        }
    }

    fn rc(&self) -> String {
        fs::read_to_string(self.home.join(".niubashrc")).unwrap_or_default()
    }

    fn spec(&self) -> String {
        fs::read_to_string(&self.spec_path).unwrap_or_default()
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self._root);
    }
}

/// Kill a child that overstayed its deadline (the "hung leg" case).
fn kill_tree(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

/// Spawn `niu <args>` under `envs` and enforce `timeout`. Stdout/stderr are
/// drained by reader threads while we poll, so a chatty child (the recipe
/// index prints >150KB) can never fill the OS pipe buffer and deadlock
/// against a wait-first reader. A hang is killed at its deadline and fails
/// the leg with a clear message instead of stalling the whole suite.
fn run_niu_timed(args: &[&str], envs: &[(&str, String)], timeout: Duration) -> Output {
    let mut command = Command::new(niu_binary());
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    apply_envs(&mut command, envs);
    let mut child = command
        .spawn()
        .unwrap_or_else(|err| panic!("spawn niu {args:?}: {err}"));
    let stdout_reader = child.stdout.take();
    let stderr_reader = child.stderr.take();
    let stdout_handle =
        std::thread::spawn(move || stdout_reader.map(read_to_end_lossy).unwrap_or_default());
    let stderr_handle =
        std::thread::spawn(move || stderr_reader.map(read_to_end_lossy).unwrap_or_default());
    let status = wait_with_deadline(&mut child, timeout, args);
    let stdout = stdout_handle
        .join()
        .unwrap_or_else(|_| panic!("stdout reader panicked for niu {args:?}"));
    let stderr = stderr_handle
        .join()
        .unwrap_or_else(|_| panic!("stderr reader panicked for niu {args:?}"));
    Output {
        status,
        stdout,
        stderr,
    }
}

fn apply_envs(command: &mut Command, envs: &[(&str, String)]) {
    for (key, value) in envs {
        // "" clears the var (NIU_ENV must be unset for boot legs).
        if value.is_empty() {
            command.env_remove(key);
        } else {
            command.env(key, value);
        }
    }
}

fn read_to_end_lossy<R: std::io::Read>(mut reader: R) -> Vec<u8> {
    let mut bytes = Vec::new();
    let _ = reader.read_to_end(&mut bytes);
    bytes
}

/// Poll a child to completion, killing it at `deadline`.
fn wait_with_deadline(
    child: &mut Child,
    timeout: Duration,
    args: &[&str],
) -> std::process::ExitStatus {
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status,
            Ok(None) if Instant::now() >= deadline => {
                kill_tree(child);
                panic!("niu {args:?} did not finish within {timeout:?}");
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(25)),
            Err(err) => panic!("wait niu {args:?}: {err}"),
        }
    }
}

fn run_niu(args: &[&str], envs: &[(&str, String)]) -> Output {
    run_niu_timed(args, envs, DEFAULT_TIMEOUT)
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

fn normalized(output: &Output) -> String {
    stdout_text(output).replace("\r\n", "\n").trim().to_string()
}

/// `niu -i` fed from a pipe (the interactive rc loads, then the piped
/// commands run; prompts render to stdout). Stdout/stderr drain on reader
/// threads for the same pipe-buffer reason as [`run_niu_timed`].
fn boot_interactive(sandbox: &Sandbox, stdin_script: &str) -> Output {
    let mut command = Command::new(niu_binary());
    command
        .arg("-i")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    apply_envs(&mut command, &sandbox.envs);
    let mut child = command
        .spawn()
        .unwrap_or_else(|err| panic!("spawn niu -i: {err}"));
    {
        let stdin = child.stdin.as_mut().expect("piped stdin");
        stdin
            .write_all(stdin_script.as_bytes())
            .and_then(|()| stdin.flush())
            .expect("write niu -i stdin");
    }
    drop(child.stdin.take());
    let stdout_reader = child.stdout.take();
    let stderr_reader = child.stderr.take();
    let stdout_handle =
        std::thread::spawn(move || stdout_reader.map(read_to_end_lossy).unwrap_or_default());
    let stderr_handle =
        std::thread::spawn(move || stderr_reader.map(read_to_end_lossy).unwrap_or_default());
    let status = wait_with_deadline(&mut child, DEFAULT_TIMEOUT, &["-i"]);
    let stdout = stdout_handle
        .join()
        .unwrap_or_else(|_| panic!("stdout reader panicked for niu -i"));
    let stderr = stderr_handle
        .join()
        .unwrap_or_else(|_| panic!("stderr reader panicked for niu -i"));
    Output {
        status,
        stdout,
        stderr,
    }
}

// ── A. install chain ─────────────────────────────────────────────────────────

/// oh-my-bash: catalog add (offline fixture) → trust → enable source+theme →
/// a fresh `niu -c '. ~/.niubashrc; echo $OSH_THEME'` sees the theme the
/// block declared. This is the literal 1.3.0 checklist leg.
#[test]
fn a1_oh_my_bash_install_chain_theme_takes_effect() {
    let sandbox = Sandbox::new("a1-omb");
    let add = run_niu(
        &[
            "plugin",
            "add",
            "oh-my-bash",
            "--path",
            &fixture("oh-my-bash").to_string_lossy(),
        ],
        &sandbox.envs,
    );
    assert_success(&add, "plugin add oh-my-bash --path");
    assert!(
        stdout_text(&add).contains("Installed source 'oh-my-bash'"),
        "{}",
        stdout_text(&add)
    );

    let trust = run_niu(&["plugin", "trust", "oh-my-bash"], &sandbox.envs);
    assert_success(&trust, "plugin trust oh-my-bash");
    let enable = run_niu(&["plugin", "enable", "oh-my-bash"], &sandbox.envs);
    assert_success(&enable, "plugin enable oh-my-bash");
    let theme = run_niu(&["plugin", "enable", "agnoster"], &sandbox.envs);
    assert_success(&theme, "plugin enable agnoster");

    let rc = sandbox.rc();
    assert!(rc.contains("OSH_THEME='agnoster'"), "{rc}");

    let probe = run_niu(&["-c", ". ~/.niubashrc; echo $OSH_THEME"], &sandbox.envs);
    assert_success(&probe, "fresh shell sourcing the managed rc");
    assert_eq!(
        normalized(&probe),
        "agnoster",
        "the enabled theme must take effect"
    );
}

/// bash-it: same chain; the theme claim surfaces through PS1 in a fresh
/// `-c` shell (the fixture theme renders `demox> `).
#[test]
fn a2_bash_it_install_chain_theme_takes_effect() {
    let sandbox = Sandbox::new("a2-bash-it");
    let add = run_niu(
        &[
            "plugin",
            "add",
            "bash-it",
            "--path",
            &fixture("bash-it").to_string_lossy(),
        ],
        &sandbox.envs,
    );
    assert_success(&add, "plugin add bash-it --path");
    let trust = run_niu(&["plugin", "trust", "bash-it"], &sandbox.envs);
    assert_success(&trust, "plugin trust bash-it");
    let plugin = run_niu(&["plugin", "enable", "base"], &sandbox.envs);
    assert_success(&plugin, "plugin enable base");
    let theme = run_niu(&["plugin", "enable", "demox"], &sandbox.envs);
    assert_success(&theme, "plugin enable demox");

    let entry = sandbox
        .sources_root
        .join("bash-it/enabled/350---base.plugin.bash");
    assert!(entry.is_file(), "bash-it enabled/ entry missing: {entry:?}");

    let probe = run_niu(
        &["-c", ". ~/.niubashrc; printf '%s' \"$PS1\""],
        &sandbox.envs,
    );
    assert_success(&probe, "fresh shell sourcing the managed rc");
    assert_eq!(
        stdout_text(&probe).trim_end_matches(['\r', '\n']),
        "demox> ",
        "the bash-it theme must claim PS1 in a fresh shell"
    );
}

/// Spec idempotency: after the OMB chain, `niu plugin sync` reports clean,
/// and a second sync leaves the spec and the rc byte-identical.
#[test]
fn a3_spec_sync_is_idempotent() {
    let sandbox = Sandbox::new("a3-sync");
    for args in [
        vec![
            "plugin",
            "add",
            "oh-my-bash",
            "--path",
            &fixture("oh-my-bash").to_string_lossy(),
        ],
        vec!["plugin", "trust", "oh-my-bash"],
        vec!["plugin", "enable", "oh-my-bash"],
        vec!["plugin", "enable", "git"],
    ] {
        let out = run_niu(&args, &sandbox.envs);
        assert_success(&out, &format!("niu {}", args.join(" ")));
    }

    let first = run_niu(&["plugin", "sync"], &sandbox.envs);
    assert_success(&first, "plugin sync");
    let spec = sandbox.spec();
    let rc = sandbox.rc();
    assert!(
        spec.contains("oh-my-bash"),
        "spec must declare the source: {spec}"
    );

    let second = run_niu(&["plugin", "sync"], &sandbox.envs);
    assert_success(&second, "second plugin sync");
    assert_eq!(
        sandbox.spec(),
        spec,
        "spec must be byte-identical after re-sync"
    );
    assert_eq!(
        sandbox.rc(),
        rc,
        "managed rc block must be byte-identical after re-sync"
    );

    // The quiet startup form is silent when everything is in sync.
    let bootstrap = run_niu(&["plugin", "sync", "--bootstrap"], &sandbox.envs);
    assert_success(&bootstrap, "plugin sync --bootstrap");
    assert!(
        stdout_text(&bootstrap).trim().is_empty(),
        "clean bootstrap must print nothing, got: {}",
        stdout_text(&bootstrap)
    );
}

// ── B. download retraction ───────────────────────────────────────────────────

/// The compiled-in recipe index lists non-empty rows (offline leg).
#[test]
fn b1_recipe_list_is_non_empty() {
    let sandbox = Sandbox::new("b1-recipes");
    let list = run_niu(&["plugin", "recipe", "list", "--json"], &sandbox.envs);
    assert_success(&list, "plugin recipe list --json");
    let text = stdout_text(&list);
    let rows: Vec<&str> = text
        .trim()
        .lines()
        .filter(|line| line.trim_start().starts_with("\"id\":"))
        .collect();
    assert!(
        rows.len() >= 2,
        "recipe index must list the ecosystem rows, got {} rows:\n{text}",
        rows.len()
    );
    assert!(text.contains("oh-my-bash"), "{text}");
}

/// Executable-tool recipes recommend package managers instead of fetching
/// (download retraction 2026-10-04): `niu plugin add fzf` exits 0, fully
/// offline, printing install commands — wpm first on Windows (owner
/// correction 2026-10-03), native package managers on every platform —
/// plus the upstream URL, and never a download attempt.
#[test]
fn b2_tool_recipes_recommend_package_managers_offline() {
    let sandbox = Sandbox::new("b2-recommend");
    let add = run_niu(&["plugin", "add", "fzf"], &sandbox.envs);
    assert_success(&add, "plugin add fzf (recommendation, offline)");
    let text = stdout_text(&add);
    assert!(
        text.contains("does not download binaries"),
        "the retraction must be stated:\n{text}"
    );
    assert!(
        text.contains("https://github.com/junegunn/fzf/"),
        "the upstream URL must be printed:\n{text}"
    );
    assert!(text.contains("sudo apt install fzf"), "{text}");
    assert!(text.contains("brew install fzf"), "{text}");
    #[cfg(windows)]
    assert!(text.contains("wpm install fzf"), "{text}");
    #[cfg(not(windows))]
    assert!(
        !text.contains("wpm"),
        "no wpm strings on non-Windows:\n{text}"
    );
    // Nothing landed anywhere: the recommendation installs nothing.
    assert!(
        !sandbox
            .home
            .join(".niubash")
            .join("tools")
            .join("fzf")
            .exists(),
        "no tool directory may be created by the recommendation"
    );
    assert!(
        !sandbox
            .home
            .join(".niubash")
            .join("tools")
            .join("registry.toml")
            .exists(),
        "no tool registry may be written"
    );
}

/// The retired downloaded-tools verbs fail loudly with the retirement
/// message (never a silent success), and `niu font` runs offline as
/// detection + recommendations.
#[test]
fn b3_retired_tool_verb_and_offline_font_recommendation() {
    let sandbox = Sandbox::new("b3-retired");

    let list = run_niu(&["plugin", "tool", "list"], &sandbox.envs);
    assert!(
        !list.status.success(),
        "plugin tool list must be retired, got:\n{}",
        stdout_text(&list)
    );
    assert!(
        stderr_text(&list).contains("retired with the download retraction"),
        "{}",
        stderr_text(&list)
    );

    // `niu font` is pure detection+recommendation: works with no terminal
    // and no network, names the package channels and nerdfonts.com.
    let font = run_niu(&["font"], &sandbox.envs);
    assert_success(&font, "niu font (offline detection+recommendation)");
    let font_text = stdout_text(&font);
    assert!(font_text.contains("JetBrainsMono Nerd Font"), "{font_text}");
    assert!(font_text.contains("nerdfonts.com"), "{font_text}");
    assert!(
        !font_text.contains("Choose a Nerd Font"),
        "no interactive install prompt may remain:\n{font_text}"
    );
    // No fonts were installed into the sandbox.
    assert!(
        !sandbox.home.join(".fonts").join("x.ttf").exists(),
        "nothing may be installed"
    );
}

// ── C. defaults-as-floor ─────────────────────────────────────────────────────

/// With an external theme enabled, the framework claims the prompt slot
/// end-to-end (boot → managed block → theme → PS1); after disable, the slot
/// is released back to the shell floor and the theme's face is gone.
#[test]
fn c1_external_theme_claims_then_releases_the_prompt_slot() {
    let sandbox = Sandbox::new("c1-floor");
    for args in [
        vec![
            "plugin",
            "add",
            "oh-my-bash",
            "--path",
            &fixture("oh-my-bash").to_string_lossy(),
        ],
        vec!["plugin", "trust", "oh-my-bash"],
        vec!["plugin", "enable", "oh-my-bash"],
        vec!["plugin", "enable", "agnoster"],
    ] {
        let out = run_niu(&args, &sandbox.envs);
        assert_success(&out, &format!("niu {}", args.join(" ")));
    }

    let claimed = boot_interactive(&sandbox, "echo CLAIM=[$PS1]\n");
    assert_success(&claimed, "interactive boot with the theme enabled");
    let claimed_out = stdout_text(&claimed);
    assert!(
        claimed_out.contains("CLAIM=[agnoster-fixture-face ]"),
        "the external theme must own PS1 after boot:\n{claimed_out}\nstderr:\n{}",
        stderr_text(&claimed)
    );

    let disable = run_niu(&["plugin", "disable", "oh-my-bash"], &sandbox.envs);
    assert_success(&disable, "plugin disable oh-my-bash");
    assert!(
        !sandbox.rc().contains(">>> niu source oh-my-bash"),
        "{}",
        sandbox.rc()
    );

    let released = boot_interactive(&sandbox, "echo CLAIM=[$PS1]\n");
    assert_success(&released, "interactive boot after disable");
    let released_out = stdout_text(&released);
    assert!(
        !released_out.contains("agnoster-fixture-face"),
        "the theme's PS1 must be gone after disable:\n{released_out}"
    );
    // On the piped `-i` path the engine initializes PS1 to GNU's default
    // `\s-\v\$ ` (variables.c set_if_not) — the floor of the shell itself.
    // The product (reedline) floor's claim/release cycle is pinned by the
    // niubash-runtime unit tests named in tests/defaults_floor.rs.
    assert!(
        released_out.contains("CLAIM=[\\s-\\v\\$ ]"),
        "after release the shell-default floor must render, got:\n{released_out}"
    );
}

// ── D. setup wizard ──────────────────────────────────────────────────────────

/// `niu setup --preset recommended` in an isolated HOME: rc written, parses
/// clean under `niu -n`, sells zero retired stack fields, journal records
/// the preset.
#[test]
fn d1_setup_preset_recommended_writes_a_clean_parseable_rc() {
    let sandbox = Sandbox::new("d1-preset");
    let setup = run_niu(&["setup", "--preset", "recommended"], &sandbox.envs);
    assert_success(&setup, "setup --preset recommended");
    assert!(
        stdout_text(&setup).contains("recommended"),
        "{}",
        stdout_text(&setup)
    );

    let rc_path = sandbox.home.join(".niubashrc");
    let rc = fs::read_to_string(&rc_path).expect("rc written");
    for dead in ["NIU_THEME=", "NIU_PLUGINS=", "NIU_BANNER="] {
        assert!(
            !rc.contains(dead),
            "retired field {dead} in preset rc:\n{rc}"
        );
    }
    assert!(rc.contains("NIU_PROMPT_CWD_STYLE"), "{rc}");
    assert!(
        rc.contains("alias ll="),
        "recommended preset ships aliases:\n{rc}"
    );

    let parse = run_niu(&["-n", &rc_path.to_string_lossy()], &sandbox.envs);
    assert_success(&parse, "niu -n parse of the generated rc");

    let journal = fs::read_to_string(sandbox.home.join(".niubash").join("setup-journal.toml"))
        .expect("journal written");
    assert!(journal.contains("preset = 'recommended'"), "{journal}");
}

// ── D. setup wizard (continued): the one-run out-of-box journey ─────────────

/// Shared ConPTY driver (portable-pty) — the same expect-style session the
/// interactive target uses, extended with `spawn_cli` for CLI subcommands.
/// `allow(dead_code)`: only a slice of the driver's surface is used here;
/// the interactive target keeps full dead-code strictness.
#[path = "interactive/driver.rs"]
#[allow(dead_code)]
mod journey_driver;

/// Recursively copy a fixture tree (the mirror seeder; git's own clone is
/// what niu exercises, this only stages the mirror's source).
fn copy_dir(src: &std::path::Path, dest: &std::path::Path) {
    fs::create_dir_all(dest).unwrap();
    for entry in fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let target = dest.join(entry.file_name());
        if entry.path().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).unwrap();
        }
    }
}

/// Seed `<dest>` as a real git repository holding the fixture tree at
/// `src` — the offline mirror niu's `git clone` resolves to.
fn seed_mirror_repo(git: &std::path::Path, src: &std::path::Path, dest: &std::path::Path) {
    copy_dir(src, dest);
    for args in [
        vec!["init", "-q"],
        vec![
            "-c",
            "user.email=smoke@niu",
            "-c",
            "user.name=niu-smoke",
            "add",
            "-A",
        ],
        vec![
            "-c",
            "user.email=smoke@niu",
            "-c",
            "user.name=niu-smoke",
            "commit",
            "-qm",
            "seed",
        ],
    ] {
        let status = Command::new(git)
            .arg("-C")
            .arg(dest)
            .args(&args)
            .output()
            .expect("git mirror seed command");
        assert!(
            status.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&status.stderr)
        );
    }
}

/// d2 — owner ruling 2026-10-03 (装完即选主题): a fresh user picks the
/// recommended collection and gets the theme pick in the SAME `niu setup`
/// run — empty gallery at Q1 → recommended → Apply → untrusted install →
/// trust-now question → pick agnoster → rc rewritten → a fresh `niu -c`
/// shows OSH_THEME=agnoster. Drives the real interactive wizard under a
/// pseudo terminal (ConPTY). Fully offline: the collection's git clones
/// resolve through a seeded local mirror (`NIU_MIRRORS` insteadOf — the
/// documented §14.8 transport layer), never the network.
#[test]
fn d2_wizard_one_run_theme_journey() {
    if !journey_driver::require_pty_or_skip("d2_wizard_one_run_theme_journey") {
        return;
    }
    let Some(git) = which("git") else {
        eprintln!("skipping: git not on PATH for the offline collection mirror");
        return;
    };
    let git_dir = git
        .parent()
        .expect("git exe has a parent dir")
        .to_path_buf();

    // The offline GitHub mirror: repo layout matches the canonical origins
    // the recipes declare (ohmybash/oh-my-bash.git, scop/bash-completion.git,
    // plus the independent recipes niubash#171 added to `recommended`:
    // rcaloras/bash-preexec.git, cykerway/complete-alias.git,
    // junegunn/fzf-git.sh.git).
    let root = temp_dir("d2-journey");
    let home = root.join("home");
    let sources = root.join("sources");
    let mirror = root.join("mirror");
    fs::create_dir_all(home.join(".niubash")).unwrap();
    fs::create_dir_all(&sources).unwrap();
    seed_mirror_repo(
        &git,
        &fixture("oh-my-bash"),
        &mirror.join("ohmybash").join("oh-my-bash.git"),
    );
    seed_mirror_repo(
        &git,
        &fixture("bash-completion"),
        &mirror.join("scop").join("bash-completion.git"),
    );
    seed_mirror_repo(
        &git,
        &fixture("bash-preexec"),
        &mirror.join("rcaloras").join("bash-preexec.git"),
    );
    seed_mirror_repo(
        &git,
        &fixture("complete-alias"),
        &mirror.join("cykerway").join("complete-alias.git"),
    );
    seed_mirror_repo(
        &git,
        &fixture("fzf-git.sh"),
        &mirror.join("junegunn").join("fzf-git.sh.git"),
    );
    let mirror_base = format!(
        "file:///{}",
        mirror.display().to_string().replace('\\', "/")
    );
    fs::write(
        home.join(".niubash").join("mirrors.toml"),
        format!(
            "# smoke journey: rewrite GitHub fetches to the seeded local mirror\n\
             schema = \"niubash:mirrors@0.1.0\"\n\
             active = \"custom\"\n\n\
             [github]\n\
             git_instead_of = \"{mirror_base}/\"\n"
        ),
    )
    .unwrap();

    let extra_env = vec![
        (
            "NIU_PLUGIN_SOURCES_ROOT".to_string(),
            sources.to_string_lossy().into_owned(),
        ),
        (
            "NIU_PLUGIN_SPEC".to_string(),
            home.join(".niubash")
                .join("plugins.toml")
                .to_string_lossy()
                .into_owned(),
        ),
        (
            "NIU_MIRRORS".to_string(),
            home.join(".niubash")
                .join("mirrors.toml")
                .to_string_lossy()
                .into_owned(),
        ),
    ];

    // One wizard run, driven like a user drives it. PATH carries git (the
    // collection install clones through it) on top of the system dirs.
    let system_root = std::env::var_os("SystemRoot")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from(r"C:\Windows"));
    let path_dirs = vec![system_root.join("System32"), system_root.clone(), git_dir];
    let mut s = journey_driver::NiuSession::spawn_cli(
        "d2-journey",
        &["setup".to_string()],
        &extra_env,
        &path_dirs,
        (120, 34),
        Duration::from_secs(120),
    );
    // Menus drop input queued while they draw (interactive_menu drains the
    // console queue after `cursor::position()` returns), so every answer
    // waits out the draw before pressing a key — send-too-early keys are
    // silently discarded and the menu would hang.
    let answer = |s: &mut journey_driver::NiuSession, keys: &str| {
        std::thread::sleep(Duration::from_millis(250));
        s.send(keys);
    };
    // Q1: the empty-ecosystem gallery note (no menu).
    s.expect("No external themes installed yet");
    // Q2.5: pick the recommended collection (displayed option 3).
    s.expect("Plugin collection?");
    answer(&mut s, "3\r");
    // Q2.6 (master): TAB completion style — the current effective style is
    // pre-highlighted, so Enter keeps it (a no-op pick writes no rc line).
    s.expect("TAB completion style?");
    answer(&mut s, "\r");
    // Q3 (Windows only): niu-git — asked once per lasting answer and skipped
    // entirely when the running installation already bundles niugit
    // (niubash#230: never re-offer what is already there). Whether it
    // renders is environment-conditional (the winuxcmd root discovery is
    // not deterministic under the cleared journey env), so the journey
    // waits for whichever question actually renders next — niu-git or the
    // Q4 provider question — and answers what it sees. A strict expect on
    // either one races the other and dead-ends the wizard.
    let q4 = "External completion providers";
    let either = std::time::Instant::now() + Duration::from_secs(60);
    loop {
        if s.screen_contains("niu-git") {
            s.expect("niu-git");
            answer(&mut s, "\r");
            break;
        }
        if s.screen_contains(q4) {
            break;
        }
        assert!(
            std::time::Instant::now() < either,
            "neither the niu-git question nor the provider question rendered; screen:\n{}",
            s.transcript()
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    // Q4: external completion providers — opt-in, default (Skip)
    // highlighted; the journey asserts the default rc stays free of the
    // carapace snippet.
    s.expect(q4);
    answer(&mut s, "\r");
    // Apply gate: Apply is the highlighted default.
    s.expect("Apply this configuration?");
    answer(&mut s, "\r");
    // Post-install trust question (the wizard question IS the trust verb).
    s.expect("to list its themes?");
    answer(&mut s, "2\r"); // Trust now
                           // The gallery built from the freshly trusted source: agnoster is the
                           // first entry after Skip.
    s.expect("Pick a theme");
    answer(&mut s, "2\r"); // agnoster
    let finish = s.expect("Change things later:");
    assert!(finish.contains("niu plugin disable agnoster"), "{finish}");
    let code = s.wait_exit();
    assert_eq!(code, 0, "the wizard run must exit 0");

    // The same run wrote the guarded activation block into the rc.
    let rc = fs::read_to_string(s.home().join(".niubashrc")).unwrap();
    assert!(rc.contains("OSH_THEME='agnoster'"), "{rc}");
    assert!(rc.contains(">>> niu source oh-my-bash"), "{rc}");
    let journal = fs::read_to_string(s.home().join(".niubash").join("setup-journal.toml")).unwrap();
    assert!(journal.contains("theme = 'agnoster'"), "{journal}");
    assert!(journal.contains("collection = 'recommended'"), "{journal}");

    // 1.3.1: the journey ends SPEC-MANAGED. The post-pick adoption
    // declared the collection's sources with the live snapshot — canonical
    // origins (the mirror is transport-only) and the picked theme.
    let spec = fs::read_to_string(home.join(".niubash").join("plugins.toml")).unwrap();
    assert!(
        spec.contains("https://github.com/ohmybash/oh-my-bash.git"),
        "oh-my-bash declared by its canonical origin: {spec}"
    );
    assert!(
        spec.contains("https://github.com/scop/bash-completion.git"),
        "bash-completion declared: {spec}"
    );
    // niubash#171: the recommended collection's independent recipe
    // entries are declared too — canonical origins, mirror transport-only.
    assert!(
        spec.contains("https://github.com/rcaloras/bash-preexec.git"),
        "bash-preexec declared: {spec}"
    );
    assert!(
        spec.contains("https://github.com/cykerway/complete-alias.git"),
        "complete-alias declared: {spec}"
    );
    assert!(
        spec.contains("https://github.com/junegunn/fzf-git.sh.git"),
        "fzf-git.sh declared: {spec}"
    );
    assert!(spec.contains("theme = 'agnoster'"), "{spec}");

    // And a fresh shell sees the theme.
    let home_str = s.home().to_string_lossy().into_owned();
    let sources_str = sources.to_string_lossy().into_owned();
    let spec_str = home
        .join(".niubash")
        .join("plugins.toml")
        .to_string_lossy()
        .into_owned();
    let probe_env: Vec<(&str, String)> = vec![
        ("HOME", home_str),
        ("USERPROFILE", s.home().to_string_lossy().into_owned()),
        ("NIU_PLUGIN_SOURCES_ROOT", sources_str),
        ("NIU_PLUGIN_SPEC", spec_str),
    ];
    let probe = run_niu(&["-c", ". ~/.niubashrc; echo $OSH_THEME"], &probe_env);
    assert_success(&probe, "fresh shell sourcing the journey rc");
    assert_eq!(
        normalized(&probe),
        "agnoster",
        "the one-run journey must leave OSH_THEME=agnoster active"
    );

    // The adopted spec round-trips: a plain sync leaves the rc untouched.
    let rc_before = fs::read_to_string(s.home().join(".niubashrc")).unwrap();
    let settle = run_niu(&["plugin", "sync"], &probe_env);
    assert_success(&settle, "plain sync after the wizard journey");
    assert_eq!(
        fs::read_to_string(s.home().join(".niubashrc")).unwrap(),
        rc_before,
        "sync after adoption must not move the rc"
    );

    let _ = fs::remove_dir_all(&root);
}

/// d3 — niubash#180: `niu setup` typed INSIDE a live session (the owner's
/// exact repro: answer the wizard, and the session still ran the old
/// configuration until a manual `source ~/.niubashrc`). The wizard is a
/// child process, so the handoff goes through a one-shot marker: the rc
/// rewrite leaves `~/.niubash/setup-apply-pending`, the finish screen
/// states the truth about the current session, and the parent session
/// consumes the marker at its next prompt — re-sourcing the new rc so the
/// freshly picked theme (a DIFFERENT theme than the session's current
/// look) renders without opening a new terminal.
#[test]
fn d3_setup_rerun_applies_in_the_live_session() {
    if !journey_driver::require_pty_or_skip("d3_setup_rerun_applies_in_the_live_session") {
        return;
    }

    let root = temp_dir("d3-journey");
    let home = root.join("home");
    let sources = root.join("sources");
    fs::create_dir_all(home.join(".niubash")).unwrap();
    fs::create_dir_all(&sources).unwrap();
    // The session's CURRENT look: the old-configuration sentinel prompt.
    let rc_text = "PS1='OLD> '\nNIU_DISABLE_DEFAULT_PLUGINS=1\n";
    fs::write(home.join(".niubashrc"), rc_text).unwrap();

    let spec_path = home.join(".niubash").join("plugins.toml");
    let envs: Vec<(&str, String)> = vec![
        ("HOME", home.to_string_lossy().into_owned()),
        ("USERPROFILE", home.to_string_lossy().into_owned()),
        (
            "NIU_PLUGIN_SOURCES_ROOT",
            sources.to_string_lossy().into_owned(),
        ),
        ("NIU_PLUGIN_SPEC", spec_path.to_string_lossy().into_owned()),
        ("NIU_ENV", String::new()),
    ];

    // A trusted fixture source, so the rerun wizard's Q1 gallery is
    // non-empty and a theme different from the current look is pickable.
    let fixture_str = fixture("oh-my-bash").to_string_lossy().into_owned();
    let add = run_niu(&["plugin", "source", "add", &fixture_str], &envs);
    assert_success(&add, "plugin source add (fixture oh-my-bash)");
    let trust = run_niu(&["plugin", "source", "trust", "oh-my-bash"], &envs);
    assert_success(&trust, "plugin source trust oh-my-bash");

    // PATH must carry the niu binary so the live session can spawn
    // `niu setup` as its child (the wizard of this journey).
    let niu_dir = niu_binary()
        .parent()
        .expect("niu binary has a parent dir")
        .to_path_buf();
    let system_root = std::env::var_os("SystemRoot")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from(r"C:\Windows"));
    let path_dirs = vec![system_root.join("System32"), system_root, niu_dir];
    let extra_env: Vec<(String, String)> = envs
        .iter()
        .map(|(key, value)| (key.to_string(), value.clone()))
        .collect();
    let mut s = journey_driver::NiuSession::spawn_shared_home(
        &root,
        rc_text,
        &extra_env,
        &path_dirs,
        (120, 34),
        Duration::from_secs(180),
    );
    s.expect("Niubash");
    s.expect("OLD> ");

    // Menus drop input queued while they draw (the d2 note), so every
    // answer waits out the draw before pressing a key.
    let answer = |s: &mut journey_driver::NiuSession, keys: &str| {
        std::thread::sleep(Duration::from_millis(250));
        s.send(keys);
    };

    // The wizard child runs on the session's console, exactly like a user
    // typing `niu setup` at the prompt.
    s.send_line("niu setup");
    // Q1 gallery (1-based jump keys): Skip is 1; themes sorted by name —
    // agnoster (the fixture's "agnoster-fixture-face " PS1) is 2, i.e. a
    // DIFFERENT theme than the session's current look.
    s.expect("Pick a theme");
    answer(&mut s, "2\r");
    // Q2.5 is skipped (the source registry is non-empty). Q2.6 (2026-10-12):
    // TAB completion style — the step runs on every `niu setup` re-run with
    // the current effective style pre-highlighted, so Enter keeps it (a
    // no-op pick writes no rc line). Without this answer the wizard waits
    // on the menu and the journey dead-ends at its 180s session timeout.
    s.expect("TAB completion style?");
    answer(&mut s, "\r");
    // Q3 (Windows only): niu-git — the default (Skip) is highlighted.
    #[cfg(windows)]
    {
        s.expect("niu-git");
        answer(&mut s, "\r");
    }
    // Q4: external completion providers — opt-in, default (Skip) highlighted;
    // the journey asserts the default rc stays free of the carapace snippet.
    s.expect("External completion providers");
    answer(&mut s, "\r");
    // Apply gate: Apply is the highlighted default.
    s.expect("Apply this configuration?");
    answer(&mut s, "\r");

    // The finish screen must state the truth about the current session
    // (niubash#180 minimum) — prominent, before the how-to-change block.
    let finish = s.expect("Change things later:");
    assert!(
        finish.contains("New config takes effect in new terminals."),
        "{finish}"
    );
    assert!(finish.contains("source ~/.niubashrc"), "{finish}");

    // The wizard child exits; the parent session consumes the handoff
    // marker at its next prompt, re-sources the new rc, and renders the
    // freshly picked theme — no new terminal opened.
    s.expect("Applied the new configuration");
    s.expect("agnoster-fixture-face ");

    // The one-shot marker is consumed, the rc carries the pick, and the
    // old sentinel prompt is gone for good.
    assert!(
        !home.join(".niubash").join("setup-apply-pending").exists(),
        "the handoff marker must be consumed by the apply"
    );
    let rc = fs::read_to_string(home.join(".niubashrc")).unwrap();
    assert!(rc.contains("OSH_THEME='agnoster'"), "{rc}");
    assert!(!rc.contains("OLD> "), "{rc}");
    // Default answer to the provider question is Skip: no snippet in the rc.
    assert!(!rc.contains("carapace"), "{rc}");

    s.send_line("exit");
    let code = s.wait_exit();
    assert_eq!(code, 0, "the session must exit cleanly after the apply");

    let _ = fs::remove_dir_all(&root);
}

// ── E. basics ────────────────────────────────────────────────────────────────

#[test]
fn e1_version_and_basic_commands() {
    let sandbox = Sandbox::new("e1-basics");

    let version = run_niu(&["--version"], &sandbox.envs);
    assert_success(&version, "niu --version");
    assert!(
        normalized(&version).to_lowercase().contains("niubash"),
        "{}",
        normalized(&version)
    );

    let hello = run_niu(&["-c", "echo hello"], &sandbox.envs);
    assert_success(&hello, "echo hello");
    assert_eq!(normalized(&hello), "hello");

    // `seq`/`wc`/`cat` are external coreutils — present under Git Bash /
    // winuxcmd on dev machines, but not guaranteed on a bare CI runner:
    // resolve them or record a skip, never a false red.
    let seq = which("seq");
    let wc = which("wc");
    let cat = which("cat");
    match (seq, wc) {
        (Some(seq), Some(wc)) => {
            let script = format!("'{}' 1 3 | '{}' -l", seq.display(), wc.display());
            let pipeline = run_niu(
                &["-c", &script],
                &[("HOME", sandbox.home.to_string_lossy().into_owned())],
            );
            assert_success(&pipeline, "seq 1 3 | wc -l");
            assert_eq!(normalized(&pipeline), "3", "pipeline count must be 3");
        }
        _ => {
            eprintln!("skipping: seq/wc not on PATH for the pipeline leg");
        }
    }

    if let Some(cat) = cat {
        let file = sandbox.home.join("lines.txt");
        fs::write(&file, "one\ntwo\nthree\n").unwrap();
        let script = format!("'{}' -n '{}'", cat.display(), file.to_string_lossy());
        let numbered = run_niu(
            &["-c", &script],
            &[("HOME", sandbox.home.to_string_lossy().into_owned())],
        );
        assert_success(&numbered, "cat -n");
        let text = normalized(&numbered);
        for (want, line) in [("1", "one"), ("2", "two"), ("3", "three")] {
            assert!(
                text.lines()
                    .any(|l| l.trim_start().starts_with(want) && l.contains(line)),
                "cat -n must number the lines, got:\n{text}"
            );
        }
    } else {
        eprintln!("skipping: cat not on PATH for the cat -n leg");
    }
}

/// Find an executable on PATH (bare name or PATHEXT on Windows).
fn which(tool: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    let exts: Vec<String> = if cfg!(windows) {
        std::env::var_os("PATHEXT")
            .map(|e| {
                std::env::split_paths(&e)
                    .map(|p| p.to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default()
    } else {
        vec![String::new()]
    };
    for dir in std::env::split_paths(&path) {
        for ext in &exts {
            let candidate = dir.join(format!("{tool}{ext}"));
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}
