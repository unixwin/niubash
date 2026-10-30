//! Defaults-as-floor (oh-my-niu design §14.5, owner ruling 2026-10-03):
//! the product's own prompt defaults are the lowest-priority *floor* — any
//! enabled external framework (oh-my-bash theme, starship init) claims the
//! prompt through PS1 and niubash never fights back, and disabling the
//! framework lets the floor render again.
//!
//! Binary-level evidence in this file:
//! * the shipped `.niubashrc.example` sells zero retired-stack fields;
//! * a real enable → boot cycle: the OMB guarded loader block claims PS1
//!   end-to-end (generated loader → `oh-my-bash.sh` → theme → PS1), and the
//!   product floor does not overwrite it;
//! * disabling the source releases the slot (fresh boot, PS1 unclaimed);
//! * the generated framework loader is sourced before floor config and user
//!   lines (niubash#359: enable never rewrites the user's rc at all), and
//!   disable drops the loader section with the rc untouched;
//! * the real oh-my-bash checkout keeps the claim-or-floor invariant under
//!   the current engine (rubash#251 gates interactive theme activation; the
//!   floor stays active meanwhile — iron law 2).
//!
//! The reedline-side backend swap (floor Template ⇄ bash-compatible channel)
//! is pinned by unit tests in `niubash-runtime::shell::tests`
//! (`prompt_claim_release_restores_the_floor`, `hook_only_prompt_command_
//! keeps_the_floor`, `starship_style_precmd_hook_claims_via_ps1_same_cycle`).
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

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

fn temp_dir(name: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("niu-floor-{name}-{}-{nanos}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// HOME + USERPROFILE + NIU_PLUGIN_SOURCES_ROOT pointed at one sandbox so
/// plugin enable/disable and rc boots never touch the real user home.
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
        let envs = vec![
            ("HOME", home.to_string_lossy().into_owned()),
            ("USERPROFILE", home.to_string_lossy().into_owned()),
            (
                "NIU_PLUGIN_SOURCES_ROOT",
                sources_root.to_string_lossy().into_owned(),
            ),
        ];
        Self {
            home,
            sources_root,
            envs,
        }
    }

    fn rc(&self) -> String {
        fs::read_to_string(self.home.join(".niubashrc")).unwrap_or_default()
    }

    /// The generated loader's text (niubash#359: the activation plane the
    /// verbs publish; the user rc is never written).
    fn loader(&self) -> String {
        fs::read_to_string(
            self.home
                .join(".niubash")
                .join("generated")
                .join("plugins.niu"),
        )
        .unwrap_or_default()
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(self.home.parent().unwrap());
    }
}

fn run_niu_with_env(args: &[&str], envs: &[(&str, String)]) -> Output {
    let mut command = Command::new(niu_binary());
    command.args(args).stdin(Stdio::null());
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

/// Run `niu -i` with piped stdin (the interactive stdin driver reads
/// commands from the pipe; rc output and echoes go to stdout). The
/// interactive rc (with the enabled framework block) is sourced first.
fn boot_interactive(sandbox: &Sandbox, stdin_data: &str) -> Output {
    let mut child = Command::new(niu_binary())
        .arg("-i")
        .env("HOME", &sandbox.home)
        .env("USERPROFILE", &sandbox.home)
        .env(
            "NIU_PLUGIN_SOURCES_ROOT",
            sandbox.sources_root.to_string_lossy().as_ref(),
        )
        .env_remove("NIU_ENV")
        .env_remove("BASH_ENV")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap_or_else(|err| panic!("failed to spawn niu -i: {err}"));
    {
        let stdin = child.stdin.as_mut().expect("piped stdin");
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

fn omb_fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("sources")
        .join("oh-my-bash")
}

fn install_omb(sandbox: &Sandbox) {
    let add = run_niu_with_env(
        &[
            "plugin",
            "add",
            "oh-my-bash",
            "--path",
            &omb_fixture().to_string_lossy(),
        ],
        &sandbox.envs,
    );
    assert_success(&add, "plugin add oh-my-bash --path");
}

/// The shipped example rc must sell the post-migration shape only: zero
/// retired-stack fields (niubash#145: NIU_THEME/NIU_THEME_PLUGIN/
/// NIU_PLUGINS/NIU_BANNER/NIUBASH + the old oh-my-niu pack chain), floor knobs
/// documented as floor-only, HOME bootstrap, and the loader/migrate-rc
/// contract (niubash#359/#499: activation lives in the generated loader —
/// the example carries NO managed-block sample for `niu plugin` to write).
#[test]
fn example_rc_sells_no_retired_stack_fields() {
    let example =
        fs::read_to_string(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".niubashrc.example"))
            .expect(".niubashrc.example is shipped");
    for dead in [
        "NIU_THEME=",
        "NIU_THEME_PLUGIN",
        "NIU_PLUGINS=",
        "NIU_BANNER",
        "NIU_PROMPT_SYMBOL",
        "NIUBASH=",
        "oh-my-niu",
    ] {
        assert!(
            !example.contains(dead),
            "retired field `{dead}` still sold: {example}"
        );
    }
    // The post-migration shape is documented: floor knobs (commented, they
    // are optional), the HOME bootstrap, and the loader contract — with NO
    // managed-block sample (niubash#499: a hand-written block would keep the
    // generated loader OFF by design).
    assert!(example.contains("NIU_PROMPT_CWD_STYLE"), "{example}");
    assert!(example.contains("NIU_COMPLETION_STYLE"), "{example}");
    assert!(example.contains("USERPROFILE"), "{example}");
    assert!(
        !example.contains(">>> niu source oh-my-bash (managed by `niu plugin enable/disable`) >>>"),
        "retired managed-block sample still taught: {example}"
    );
    assert!(
        !example.contains("<<< niu source oh-my-bash <<<"),
        "retired managed-block sample still taught: {example}"
    );
    // The current model (niubash#359): manifest + generated loader, the
    // rollback switch, and the read-only migration verb for legacy blocks.
    assert!(example.contains("plugins.toml"), "{example}");
    assert!(example.contains("generated/plugins.niu"), "{example}");
    assert!(example.contains("NIU_PLUGIN_LOADER"), "{example}");
    assert!(example.contains("migrate-rc"), "{example}");
    // The example must stay parseable under the engine (niubash#157 guards
    // the closed `${USERPROFILE//\\//}` form; a broken expansion silently
    // disables every alias in an adopted rc).
    let parsed = run_niu_with_env(
        &[
            "-n",
            &PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join(".niubashrc.example")
                .to_string_lossy(),
        ],
        &[],
    );
    assert_success(&parsed, "noexec parse of .niubashrc.example");
}

/// A freshly enabled oh-my-bash theme claims the prompt end-to-end: the
/// generated loader (host-sourced before the rc, niubash#359) loads
/// `oh-my-bash.sh`, the theme sets PS1, and the boot shell carries the
/// claim — the product floor never overwrites it.
#[test]
fn omb_theme_claims_the_prompt_slot_end_to_end() {
    let sandbox = Sandbox::new("omb-claim");
    install_omb(&sandbox);
    for target in ["oh-my-bash", "agnoster"] {
        let enable = run_niu_with_env(&["plugin", "enable", target], &sandbox.envs);
        assert_success(&enable, &format!("enable {target}"));
    }
    let loader = sandbox.loader();
    assert!(loader.contains("OSH_THEME='agnoster'"), "{loader}");
    assert!(!sandbox.rc().contains("OSH_THEME"), "{}", sandbox.rc());

    let output = boot_interactive(&sandbox, "echo CLAIM=[$PS1]\n");
    assert_success(&output, "boot with enabled OMB theme");
    let stdout = stdout_text(&output);
    assert!(
        stdout.contains("CLAIM=[agnoster-fixture-face ]"),
        "expected the OMB theme's PS1 to survive into the shell, got:\n{stdout}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Disabling the framework releases the prompt slot: a fresh boot runs with
/// PS1 unclaimed, so the product floor renders again (the floor render
/// itself is pinned by the runtime unit tests; this leg pins the rc side —
/// no block, no claim).
#[test]
fn disabling_the_source_releases_the_prompt_slot() {
    let sandbox = Sandbox::new("omb-release");
    install_omb(&sandbox);
    for target in ["oh-my-bash", "agnoster"] {
        let enable = run_niu_with_env(&["plugin", "enable", target], &sandbox.envs);
        assert_success(&enable, &format!("enable {target}"));
    }
    let disable = run_niu_with_env(&["plugin", "disable", "oh-my-bash"], &sandbox.envs);
    assert_success(&disable, "disable oh-my-bash");
    assert!(!sandbox.rc().contains(">>> niu source oh-my-bash"));

    let output = boot_interactive(&sandbox, "echo CLAIM=[$PS1]\n");
    assert_success(&output, "boot after disable");
    let stdout = stdout_text(&output);
    assert!(
        !stdout.contains("agnoster-fixture-face"),
        "the framework's PS1 must be gone after disable, got:\n{stdout}"
    );
    // On the piped `-i` path the ENGINE renders prompts and initializes PS1
    // to GNU's default `\s-\v\$ ` itself (variables.c:572 set_if_not; the
    // reedline product floor is not in play on this path). The product
    // floor's own restore is pinned by the runtime unit test
    // `prompt_claim_release_restores_the_floor`; here we pin the rc side:
    // no managed block, no framework claim, prompt back at a shell default.
    assert!(
        stdout.contains("CLAIM=[") && !stdout.contains("CLAIM=[agnoster"),
        "expected a shell-default PS1 after release, got:\n{stdout}"
    );
}

/// rc order contract (§14.5), read-only edition (niubash#359): enable never
/// rewrites the user's rc at all — floor knobs and user lines stay exactly
/// where the user put them, byte for byte — and the framework activation
/// lands in the generated loader, which the host sources BEFORE the rc so
/// the user's file still wins the ordering.
#[test]
fn enable_never_rewrites_the_rc_and_the_loader_carries_the_framework() {
    let sandbox = Sandbox::new("rc-order");
    install_omb(&sandbox);
    let user_rc = "# my rc\nNIU_PROMPT_CWD_STYLE='home'\nalias ll='ls -la'\n";
    fs::write(sandbox.home.join(".niubashrc"), user_rc).unwrap();

    let enable = run_niu_with_env(&["plugin", "enable", "oh-my-bash"], &sandbox.envs);
    assert_success(&enable, "enable oh-my-bash");
    let rc = sandbox.rc();
    assert_eq!(rc, user_rc, "enable must not touch the rc (niubash#359)");
    let loader = sandbox.loader();
    assert!(
        loader.contains("oh-my-bash.sh"),
        "the framework activation is in the loader: {loader}"
    );

    let disable = run_niu_with_env(&["plugin", "disable", "oh-my-bash"], &sandbox.envs);
    assert_success(&disable, "disable oh-my-bash");
    assert_eq!(
        sandbox.rc(),
        user_rc,
        "disable must not touch the rc either"
    );
    assert!(
        !sandbox.loader().contains("oh-my-bash.sh"),
        "the loader section is dropped: {}",
        sandbox.loader()
    );
}

/// The real oh-my-bash checkout (82 themes, full lib chain) must keep the
/// claim-or-floor invariant under the current engine: sourcing the guarded
/// loader either claims PS1 (engine interactive chain green) or leaves it
/// empty (rubash#251 gate) — either way the boot succeeds and the floor
/// stays reachable. Runs only where the corpus checkout exists.
#[test]
fn real_ohmybash_chain_keeps_claim_or_floor_invariant() {
    let corpus = PathBuf::from("D:/repo/rubash/target-ecosys/repos/oh-my-bash");
    if !corpus.join("oh-my-bash.sh").is_file() {
        eprintln!(
            "skipping: real oh-my-bash corpus checkout not present at {}",
            corpus.display()
        );
        return;
    }
    let sandbox = Sandbox::new("real-omb");
    // A user rc that adopts the real tree through the same guarded shape
    // `niu plugin enable` writes (OSH export + readability guard).
    fs::create_dir_all(&sandbox.sources_root).unwrap();
    // Point the loader at the real corpus tree: OSH is the tree root, bound
    // directly at the corpus path inside the guarded block.
    let loader = format!(
        "export DISABLE_AUTO_UPDATE=true\nOSH='{0}'\nif [ -f \"$OSH/oh-my-bash.sh\" ]; then\n  OSH_THEME=agnoster\n  . \"$OSH/oh-my-bash.sh\"\nfi\n",
        corpus.to_string_lossy().replace('\\', "/")
    );
    fs::write(sandbox.home.join(".niubashrc"), loader).unwrap();

    let output = boot_interactive(
        &sandbox,
        "case \"${PS1:-}\" in '') echo SLOT=UNCLAIMED;; *) echo SLOT=CLAIMED;; esac\n",
    );
    assert!(
        output.status.success(),
        "boot sourcing the real oh-my-bash chain must not fail:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = stdout_text(&output);
    assert!(
        stdout.contains("SLOT=CLAIMED") || stdout.contains("SLOT=UNCLAIMED"),
        "claim-or-floor invariant marker missing:\n{stdout}"
    );
    // Whichever side of rubash#251 we are on, record it for the log.
    let side = if stdout.contains("SLOT=CLAIMED") {
        "claimed (engine chain green)"
    } else {
        "unclaimed (floor active; rubash#251 gate)"
    };
    eprintln!("real oh-my-bash boot: slot {side}");
}
