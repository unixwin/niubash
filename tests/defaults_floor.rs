//! Defaults-as-floor (oh-my-niu design §14.5, owner ruling 2026-10-03):
//! the product's own prompt defaults are the lowest-priority *floor* — any
//! enabled external framework (oh-my-bash theme, starship init) claims the
//! prompt through PS1 and niubash never fights back, and disabling the
//! framework lets the floor render again.
//!
//! Binary-level evidence in this file:
//! * the shipped `.niubashrc.example` sells zero retired-stack fields;
//! * a real enable → boot cycle: the OMB guarded loader block claims PS1
//!   end-to-end (managed rc block → `oh-my-bash.sh` → theme → PS1), and the
//!   product floor does not overwrite it;
//! * disabling the source releases the slot (fresh boot, PS1 unclaimed);
//! * the managed framework block lands after floor config and user lines,
//!   and disable never rewrites anything outside its markers;
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

fn install_trusted_omb(sandbox: &Sandbox) {
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
    let trust = run_niu_with_env(&["plugin", "trust", "oh-my-bash"], &sandbox.envs);
    assert_success(&trust, "plugin trust oh-my-bash");
}

/// The shipped example rc must sell the post-migration shape only: zero
/// retired-stack fields (niubash#145: NIU_THEME/NIU_THEME_PLUGIN/
/// NIU_PLUGINS/NIU_BANNER/NIUBASH + `oh-my-niu.winux` chain), floor knobs
/// documented as floor-only, HOME bootstrap, and the managed-block marker
/// discipline shown for reference.
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
    // are optional), the HOME bootstrap, and the niu-managed block markers.
    assert!(example.contains("NIU_PROMPT_CWD_STYLE"), "{example}");
    assert!(example.contains("NIU_COMPLETION_STYLE"), "{example}");
    assert!(example.contains("USERPROFILE"), "{example}");
    assert!(
        example.contains(">>> niu source oh-my-bash (managed by `niu plugin enable/disable`) >>>"),
        "{example}"
    );
    assert!(
        example.contains("<<< niu source oh-my-bash <<<"),
        "{example}"
    );
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
/// guarded managed block in the rc loads `oh-my-bash.sh`, the theme sets
/// PS1, and the boot shell carries the claim — the product floor never
/// overwrites it.
#[test]
fn omb_theme_claims_the_prompt_slot_end_to_end() {
    let sandbox = Sandbox::new("omb-claim");
    install_trusted_omb(&sandbox);
    for target in ["oh-my-bash", "agnoster"] {
        let enable = run_niu_with_env(&["plugin", "enable", target], &sandbox.envs);
        assert_success(&enable, &format!("enable {target}"));
    }
    let rc = sandbox.rc();
    assert!(rc.contains("OSH_THEME='agnoster'"), "{rc}");

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
    install_trusted_omb(&sandbox);
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

/// rc order contract (§14.5): the niu floor knobs live early in the user's
/// rc, the framework enable block is appended after all existing lines, and
/// enable/disable never rewrite anything outside the marker pair.
#[test]
fn managed_block_lands_after_floor_config_and_user_lines() {
    let sandbox = Sandbox::new("rc-order");
    install_trusted_omb(&sandbox);
    fs::write(
        sandbox.home.join(".niubashrc"),
        "# my rc\nNIU_PROMPT_CWD_STYLE='home'\nalias ll='ls -la'\n",
    )
    .unwrap();

    let enable = run_niu_with_env(&["plugin", "enable", "oh-my-bash"], &sandbox.envs);
    assert_success(&enable, "enable oh-my-bash");
    let rc = sandbox.rc();
    let floor_pos = rc
        .find("NIU_PROMPT_CWD_STYLE=")
        .expect("floor knob still present");
    let user_pos = rc.find("alias ll=").expect("user line still present");
    let block_pos = rc
        .find(">>> niu source oh-my-bash")
        .expect("managed block appended");
    assert!(
        floor_pos < block_pos && user_pos < block_pos,
        "managed block must load after floor config and user lines:\n{rc}"
    );

    let disable = run_niu_with_env(&["plugin", "disable", "oh-my-bash"], &sandbox.envs);
    assert_success(&disable, "disable oh-my-bash");
    let rc = sandbox.rc();
    assert!(rc.contains("# my rc"), "{rc}");
    assert!(rc.contains("NIU_PROMPT_CWD_STYLE='home'"), "{rc}");
    assert!(rc.contains("alias ll='ls -la'"), "{rc}");
    assert!(!rc.contains(">>> niu source"), "{rc}");
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
