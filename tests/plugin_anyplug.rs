//! Any-plugin acceptance set (design §14.6.4, wt46/anyplug): the three
//! working layers over *arbitrary* bash content —
//!
//! * base layer: wild single-file / gist-style repos installed as file
//!   sources with honest candidate enumeration (no guessed entry);
//! * manager layer: descriptor-driven managers, including the bpkg
//!   adapter (manifest `scripts` array, per-package ids, npm-shaped
//!   `package.json` rejected);
//! * red line: framework-dependent plugins stay byte-faithful — directly
//!   sourcing bashmarks fails with the same "command not found" you get
//!   under GNU bash, because nothing shims `_omb_module_require` (§14.4).
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
                    .join("plugins.json")
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

    /// The generated loader's text (niubash#359: the activation plane the
    /// verbs publish; the user rc is never written).
    fn rc(&self) -> String {
        fs::read_to_string(
            self.home
                .join(".niubash")
                .join("generated")
                .join("plugins.niu"),
        )
        .unwrap_or_default()
    }

    fn spec(&self) -> String {
        fs::read_to_string(self.home.join(".niubash").join("plugins.json")).unwrap_or_default()
    }
}

/// §14.6.4 row 1 — single-file wild plugin: add → honest
/// candidate listing → enable by picking the file → the engine sources it
/// and the function works. Nothing is guessed; the README is not a
/// candidate; the file keeps its `script` tag in the listing.
#[test]
fn wild_single_file_plugin_enumerates_honestly_and_loads() {
    let sandbox = Sandbox::new("wild-single");
    let origin = fixture("wild-spark");

    let add = run_niu_with_env(&["plugin", "add", &origin.to_string_lossy()], &sandbox.envs);
    assert_success(&add, "plugin add <path>");
    let add_out = stdout_text(&add);
    assert!(
        add_out.contains("Installed source 'wild-spark'"),
        "{add_out}"
    );
    assert!(
        add_out.contains("niu plugin enable wild-spark"),
        "{add_out}"
    );

    // Honest enumeration: the one sourceable file, tagged; README absent.
    let list = run_niu_with_env(&["plugin", "list"], &sandbox.envs);
    let list_out = stdout_text(&list);
    assert!(list_out.contains("spark.bash [script]"), "{list_out}");
    assert!(!list_out.contains("README"), "{list_out}");

    // Source-level enable refuses to guess an entry for a file source.
    let guess = run_niu_with_env(&["plugin", "enable", "wild-spark"], &sandbox.envs);
    assert!(!guess.status.success(), "no guessed entry");
    assert!(
        stderr_text(&guess).contains("pick one"),
        "{}",
        stderr_text(&guess)
    );

    // Per-file enable writes one guarded source line and the spec records
    // the tree-relative path as the asset name.
    assert_success(
        &run_niu_with_env(
            &["plugin", "enable", "wild-spark/spark.bash"],
            &sandbox.envs,
        ),
        "enable spark.bash",
    );
    let rc = sandbox.rc();
    assert!(
        rc.contains(
            ". \"${NIU_PLUGIN_SOURCES_ROOT:-$HOME/.niubash/sources}/wild-spark/spark.bash\""
        ),
        "{rc}"
    );
    assert!(
        sandbox.spec().contains("\"spark.bash\""),
        "{}",
        sandbox.spec()
    );

    // Engine proof: sourcing the rc defines the function and it runs.
    let engine = run_niu_with_env(
        &[
            "-c",
            &format!(
                ". {}; spark loaded-ok",
                sandbox
                    .home
                    .join(".niubash")
                    .join("generated")
                    .join("plugins.niu")
                    .to_string_lossy()
            ),
        ],
        &sandbox.envs,
    );
    assert_success(&engine, "engine wild load");
    assert_eq!(stdout_text(&engine).trim_end(), "spark! loaded-ok");

    let _ = fs::remove_dir_all(sandbox.home.parent().unwrap());
}

/// §14.6.4 row 2 — bash-preexec: the canonical wild single-file plugin.
/// Loading through the managed rc block must produce shell state identical
/// to manually sourcing the file (byte-faithful loading), and the
/// preexec/precmd hook machinery must work exactly as it does after a
/// manual source.
#[test]
fn bash_preexec_loads_identically_to_manual_sourcing() {
    let sandbox = Sandbox::new("bash-preexec");
    let origin = fixture("bash-preexec");

    let add = run_niu_with_env(&["plugin", "add", &origin.to_string_lossy()], &sandbox.envs);
    assert_success(&add, "plugin add");
    assert!(
        stdout_text(&add).contains("Installed source 'bash-preexec'"),
        "{}",
        stdout_text(&add)
    );
    assert_success(
        &run_niu_with_env(
            &["plugin", "enable", "bash-preexec/bash-preexec.sh"],
            &sandbox.envs,
        ),
        "enable bash-preexec.sh",
    );

    let file = sandbox
        .sources_root
        .join("bash-preexec/bash-preexec.sh")
        .to_string_lossy()
        .into_owned();
    let rc = sandbox
        .home
        .join(".niubash")
        .join("generated")
        .join("plugins.niu")
        .to_string_lossy()
        .into_owned();

    // Byte-faithful loading: sourcing the file directly and sourcing it
    // through the generated loader must yield the same hook state.
    let probe = r#"printf '%s|%s|%s' "${PROMPT_COMMAND-}" "$(type -t __bp_run_precmd)" "${#precmd_functions[@]}""#;
    let manual = run_niu_with_env(&["-c", &format!(". {file}; {probe}")], &sandbox.envs);
    assert_success(&manual, "manual source");
    let via_rc = run_niu_with_env(&["-c", &format!(". {rc}; {probe}")], &sandbox.envs);
    assert_success(&via_rc, "rc-block source");
    assert_eq!(
        stdout_text(&manual),
        stdout_text(&via_rc),
        "managed loading must equal manual sourcing byte-for-byte"
    );
    assert!(
        stdout_text(&via_rc).contains("__bp_run_precmd"),
        "PROMPT_COMMAND wrapper registered: {}",
        stdout_text(&via_rc)
    );

    // Hook behavior: a registered precmd function runs exactly once per
    // __bp_run_precmd call, same as upstream after a manual source.
    let hook = format!(
        ". {rc}; hook_ran=0; my_hook() {{ hook_ran=$((hook_ran+1)); }}; \
         precmd_functions+=(my_hook); __bp_run_precmd; printf '%s' \"$hook_ran\""
    );
    let engine = run_niu_with_env(&["-c", &hook], &sandbox.envs);
    assert_success(&engine, "precmd hook run");
    assert_eq!(stdout_text(&engine).trim_end(), "1");

    let _ = fs::remove_dir_all(sandbox.home.parent().unwrap());
}

/// §14.6.4 row 3 — bpkg: adopt a `bpkg install`-shaped tree through
/// `--path` (appendix A WP-S3: niu never downloads; the user's bpkg did).
/// The manifest's `scripts` array drives candidate enumeration, the id is
/// per-package (the tree name, not "bpkg"), and an npm-shaped
/// `package.json` (scripts as an object) never matches the bpkg
/// fingerprint.
#[test]
fn bpkg_package_adoption_uses_manifest_scripts_and_per_package_ids() {
    let sandbox = Sandbox::new("bpkg");
    let pkg = fixture("bpkg/mypkg");

    // Explicit kind adopts the tree; the id derives from the tree name
    // (per-install scope), not the manager id.
    let add = run_niu_with_env(
        &["plugin", "add", "bpkg", "--path", &pkg.to_string_lossy()],
        &sandbox.envs,
    );
    assert_success(&add, "plugin add bpkg --path");
    let add_out = stdout_text(&add);
    assert!(
        add_out.contains("Installed source 'mypkg'"),
        "per-package id: {add_out}"
    );
    assert!(
        !add_out.contains("Installed source 'bpkg'"),
        "manager id must not be the install id: {add_out}"
    );
    let spec = sandbox.spec();
    assert!(
        spec.contains("\"target\"") && !spec.contains("\"id\": \"bpkg\""),
        "spec entry leaves the id to sync (derived per package): {spec}"
    );

    // Manifest scripts are the candidates (tree-relative paths, tagged).
    let list = run_niu_with_env(&["plugin", "list"], &sandbox.envs);
    let list_out = stdout_text(&list);
    assert!(
        list_out.contains("bin/mypkg.sh [bpkg script]"),
        "{list_out}"
    );
    assert!(
        list_out.contains("lib/helper.sh [bpkg script]"),
        "{list_out}"
    );

    // Enable one manifest script; the engine runs it and the function the
    // package exports works (including its internal relative source).
    assert_success(
        &run_niu_with_env(&["plugin", "enable", "mypkg/bin/mypkg.sh"], &sandbox.envs),
        "enable manifest script",
    );
    let rc = sandbox
        .home
        .join(".niubash")
        .join("generated")
        .join("plugins.niu")
        .to_string_lossy()
        .into_owned();
    let engine = run_niu_with_env(
        &[
            "-c",
            &format!(
                "BPKG_ROOT='{}'; . {rc}; mypkg x y",
                sandbox
                    .sources_root
                    .join("mypkg")
                    .to_string_lossy()
                    .into_owned()
                    .replace('\\', "/")
            ),
        ],
        &sandbox.envs,
    );
    assert_success(&engine, "engine bpkg load");
    assert_eq!(stdout_text(&engine).trim_end(), "mypkg:helper(x y)");

    // npm shape: `scripts` as an object never matches the bpkg
    // fingerprint — the explicit kind refuses, and auto-detect falls
    // through to the wild file-source layer.
    let npm = fixture("npm-shape");
    let refused = run_niu_with_env(
        &["plugin", "add", "bpkg", "--path", &npm.to_string_lossy()],
        &sandbox.envs,
    );
    assert!(
        !refused.status.success(),
        "npm-shaped tree must not be adopted as bpkg"
    );
    assert!(
        stderr_text(&refused).contains("does not look like 'bpkg'"),
        "{}",
        stderr_text(&refused)
    );

    let _ = fs::remove_dir_all(sandbox.home.parent().unwrap());
}

/// §14.6.4 row 4 — bashmarks, the framework-dependent plugin, pins the
/// §14.4 no-shim red line:
///
/// * directly sourcing the plugin fails with the same "command not found"
///   GNU bash produces for the missing `_omb_module_require` — niubash
///   must not mask or shim it;
/// * with the framework present (what the oh-my-bash loader provides),
///   the very same file sources cleanly;
/// * enabling through the manager's own mechanism (the `plugins=()` rc
///   array + guarded loader) is the supported path.
#[test]
fn bashmarks_keeps_manual_source_fidelity_no_shim() {
    let sandbox = Sandbox::new("bashmarks");
    let origin = fixture("omb-bashmarks");

    let add = run_niu_with_env(&["plugin", "add", &origin.to_string_lossy()], &sandbox.envs);
    assert_success(&add, "plugin add omb-bashmarks fixture");
    assert!(
        stdout_text(&add).contains("Installed source 'oh-my-bash'"),
        "manager fingerprint wins for manager-shaped trees: {}",
        stdout_text(&add)
    );

    // The supported path first: enable bashmarks through the manager's
    // own selection mechanism (the loader's plugins array in the managed
    // rc block).
    assert_success(
        &run_niu_with_env(&["plugin", "enable", "bashmarks"], &sandbox.envs),
        "enable bashmarks",
    );
    let rc = sandbox.rc();
    assert!(rc.contains("plugins=('bashmarks')"), "{rc}");
    assert!(rc.contains(". \"$OSH/oh-my-bash.sh\""), "{rc}");

    // Negative fidelity (§14.4): directly sourcing the plugin without the
    // framework fails exactly like under GNU bash — a "command not found"
    // diagnostic naming the missing framework function, and the same exit
    // status (0: `.` returns the file's last command, a function
    // definition — checked against GNU bash below). No shim, no masking.
    let plugin = sandbox
        .sources_root
        .join("oh-my-bash/plugins/bashmarks/bashmarks.plugin.sh")
        .to_string_lossy()
        .into_owned()
        .replace('\\', "/");
    let bare = run_niu_with_env(&["-c", &format!(". '{plugin}'")], &sandbox.envs);
    assert_eq!(
        bare.status.code(),
        Some(0),
        "same exit as GNU bash (last command in the file is a definition)"
    );
    let bare_err = stderr_text(&bare);
    assert!(
        bare_err.contains("command not found"),
        "same error class as GNU bash: {bare_err}"
    );
    assert!(
        bare_err.contains("_omb_module_require"),
        "the missing framework function is named: {bare_err}"
    );

    // Cross-shell wording check (best effort, skipped where no bash is on
    // PATH): GNU bash reports the same function in the same diagnostic
    // shape, and the same exit status.
    if let Ok(bash) = which_bash() {
        let gnu = Command::new(&bash)
            .arg("-c")
            .arg(format!(". '{plugin}'"))
            .envs(sandbox.envs.iter().cloned())
            .output();
        if let Ok(gnu) = gnu {
            let gnu_err = stderr_text(&gnu);
            assert!(
                gnu_err.contains("_omb_module_require") && gnu_err.contains("command not found"),
                "GNU baseline: {gnu_err}"
            );
            assert_eq!(
                gnu.status.code(),
                bare.status.code(),
                "exit statuses must match: bash {} vs niu {}",
                gnu.status,
                bare.status
            );
        }
    }

    // Positive control: with the framework function present (what the
    // oh-my-bash loader defines), the very same file sources cleanly and
    // defines its API.
    let with_framework = format!(
        "_omb_module_require() {{ :; }}; OSH='{}'; . {plugin}; printf '%s' \"$(type -t jump)\"",
        sandbox
            .sources_root
            .join("oh-my-bash")
            .to_string_lossy()
            .into_owned()
            .replace('\\', "/")
    );
    let engine = run_niu_with_env(&["-c", &with_framework], &sandbox.envs);
    assert_success(&engine, "framework-present source");
    assert_eq!(stdout_text(&engine).trim_end(), "function");

    let _ = fs::remove_dir_all(sandbox.home.parent().unwrap());
}

/// Locate a GNU-ish bash for the best-effort fidelity comparison (Git
/// Bash on Windows, plain bash elsewhere). Absent → skip the comparison.
fn which_bash() -> Result<PathBuf, ()> {
    let candidates: Vec<PathBuf> = if cfg!(windows) {
        vec![
            PathBuf::from("C:/Program Files/Git/bin/bash.exe"),
            PathBuf::from("C:/Program Files (x86)/Git/bin/bash.exe"),
            PathBuf::from("D:/Git/bin/bash.exe"),
        ]
    } else {
        vec![PathBuf::from("/bin/bash"), PathBuf::from("/usr/bin/bash")]
    };
    for candidate in candidates {
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    // PATH lookup as a last resort.
    if let Ok(path) = std::env::var("PATH") {
        for dir in path.split(if cfg!(windows) { ';' } else { ':' }) {
            let candidate =
                PathBuf::from(dir).join(if cfg!(windows) { "bash.exe" } else { "bash" });
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
    }
    Err(())
}

/// §14.6.4 row 5 — unknown-shape repo: a README + installer is not a
/// plugin layout. The installer is LISTED with its warning tag (never
/// hidden, never guessed as "the" entry), nothing is auto-sourced, and a
/// repo with nothing sourceable is refused honestly.
#[test]
fn unknown_shape_repos_list_tagged_candidates_or_fail_honestly() {
    let sandbox = Sandbox::new("unknown-shape");
    let origin = fixture("unknown-shape");

    let add = run_niu_with_env(&["plugin", "add", &origin.to_string_lossy()], &sandbox.envs);
    assert_success(&add, "plugin add unknown-shape");

    // install.sh is a candidate — visible, tagged as installer-like.
    let list = run_niu_with_env(&["plugin", "list"], &sandbox.envs);
    let list_out = stdout_text(&list);
    assert!(
        list_out.contains("install.sh [installer/test-like"),
        "tagged candidate must be listed: {list_out}"
    );
    // Nothing was sourced by default: no rc was ever written and the
    // loader generation carries no sections.
    assert!(
        !sandbox.rc().contains("# --- niu plugin source:"),
        "no section may exist: {}",
        sandbox.rc()
    );

    // Source-level enable refuses to guess.
    let guess = run_niu_with_env(&["plugin", "enable", "unknown-shape"], &sandbox.envs);
    assert!(!guess.status.success(), "no guessed entry");
    assert!(
        stderr_text(&guess).contains("pick one"),
        "{}",
        stderr_text(&guess)
    );

    // A repo with nothing sourceable at all fails at add time, honestly.
    let none = fixture("no-sourceable");
    let refused = run_niu_with_env(&["plugin", "add", &none.to_string_lossy()], &sandbox.envs);
    assert!(
        !refused.status.success(),
        "nothing to source must fail the add"
    );
    assert!(
        stderr_text(&refused).contains("no sourceable"),
        "{}",
        stderr_text(&refused)
    );

    let _ = fs::remove_dir_all(sandbox.home.parent().unwrap());
}
