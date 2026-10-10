//! Regression tests for the niubash#176 add/rc audit P2 batch:
//!
//! * `niu plugin add -h` is a usage request, not an unknown-option error;
//! * `niu plugin --help` mentions `--url`;
//! * a local directory that happens to be named `oh-my-bash` is adopted as
//!   a local tree, never hijacked to the catalog's GitHub origin;
//! * a cwd-relative path target is stored cwd-independent (absolute) in the
//!   spec;
//! * the already-declared refusal names a removal path that works for the
//!   state it names (`sync --prune` for a stranded declaration).
use std::fs;
use std::path::{Path, PathBuf};
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

/// A vendored oh-my-bash-shaped local tree: adopting it must not need the
/// network (that IS the hijack assertion — the catalog origin would clone).
fn write_local_omb(root: &Path) {
    fs::create_dir_all(root.join("themes/agnoster")).unwrap();
    fs::create_dir_all(root.join("plugins/git")).unwrap();
    fs::write(
        root.join("oh-my-bash.sh"),
        "#!/usr/bin/env bash\ncase $- in *i*) ;; *) return;; esac\n",
    )
    .unwrap();
    fs::write(
        root.join("themes/agnoster/agnoster.theme.sh"),
        "PS1='agnoster> '\n",
    )
    .unwrap();
    fs::write(
        root.join("plugins/git/git.plugin.sh"),
        "alias gg='git status'\n",
    )
    .unwrap();
}

struct Sandbox {
    home: PathBuf,
    sources_root: PathBuf,
    envs: Vec<(&'static str, String)>,
    temp: PathBuf,
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
            temp,
        }
    }

    fn run(&self, args: &[&str]) -> Output {
        self.run_in(args, &self.temp)
    }

    fn run_in(&self, args: &[&str], cwd: &Path) -> Output {
        let mut command = Command::new(niu_binary());
        command.args(args);
        command.current_dir(cwd);
        for (key, value) in &self.envs {
            command.env(key, value);
        }
        command
            .output()
            .unwrap_or_else(|err| panic!("failed to run niu {args:?}: {err}"))
    }

    fn spec(&self) -> String {
        fs::read_to_string(self.home.join(".niubash").join("plugins.toml")).unwrap_or_default()
    }
}

#[test]
fn plugin_add_h_is_a_usage_request() {
    let box_ = Sandbox::new("add-h");
    for flag in ["-h", "--help"] {
        let output = box_.run(&["plugin", "add", flag]);
        assert!(
            output.status.success(),
            "add {flag} must succeed: {} {}",
            stdout_text(&output),
            stderr_text(&output)
        );
        let stdout = stdout_text(&output);
        assert!(stdout.contains("Usage:"), "{stdout}");
        assert!(
            stdout.contains("--url"),
            "the add usage shows --url: {stdout}"
        );
        assert!(
            !stderr_text(&output).contains("unknown plugin source option"),
            "not an unknown-option error: {}",
            stderr_text(&output)
        );
    }
    let _ = fs::remove_dir_all(&box_.temp);
}

#[test]
fn plugin_usage_mentions_url_option() {
    let box_ = Sandbox::new("usage-url");
    let output = box_.run(&["plugin", "--help"]);
    assert!(output.status.success());
    let stdout = stdout_text(&output);
    assert!(stdout.contains("--url"), "usage shows --url: {stdout}");
    let _ = fs::remove_dir_all(&box_.temp);
}

/// The hijack (niubash#176): a cwd directory literally named `oh-my-bash`
/// is adopted as a LOCAL tree — the spec target is its absolute path (not
/// the raw relative spelling, not the catalog's GitHub URL) and the whole
/// add succeeds offline, which a GitHub clone could not.
#[test]
fn local_dir_named_like_a_catalog_id_is_not_hijacked() {
    let box_ = Sandbox::new("no-hijack");
    let cwd = box_.temp.join("project");
    fs::create_dir_all(&cwd).unwrap();
    write_local_omb(&cwd.join("oh-my-bash"));

    let output = box_.run_in(&["plugin", "add", "oh-my-bash"], &cwd);
    assert!(
        output.status.success(),
        "a local tree adds offline (no network hijack): {} {}",
        stdout_text(&output),
        stderr_text(&output)
    );
    let spec = box_.spec();
    let expected = cwd.join("oh-my-bash").to_string_lossy().replace('\\', "/");
    assert!(
        spec.contains(&expected),
        "the spec target is the absolute local path {expected}: {spec}"
    );
    assert!(
        !spec.contains("github.com"),
        "the catalog origin never wins: {spec}"
    );
    assert!(
        !spec.contains("target = 'oh-my-bash'"),
        "not the raw relative spelling either: {spec}"
    );

    // And the tree really was installed from the local directory.
    let installed = fs::read_dir(&box_.sources_root).unwrap().count();
    assert!(installed >= 1, "the local tree is installed");
    let _ = fs::remove_dir_all(&box_.temp);
}

/// A cwd-relative path spelling is normalized to an absolute target in the
/// spec, so a later sync from another directory resolves the same tree.
#[test]
fn relative_path_targets_are_stored_absolute() {
    let box_ = Sandbox::new("relative-target");
    let cwd = box_.temp.join("project");
    fs::create_dir_all(&cwd).unwrap();
    write_local_omb(&cwd.join("myplug"));

    let output = box_.run_in(&["plugin", "add", "./myplug"], &cwd);
    assert!(
        output.status.success(),
        "{} {}",
        stdout_text(&output),
        stderr_text(&output)
    );
    let spec = box_.spec();
    let expected = cwd.join("myplug").to_string_lossy().replace('\\', "/");
    assert!(
        spec.contains(&expected),
        "the spec target is absolute {expected}: {spec}"
    );
    assert!(
        !spec.contains("./myplug"),
        "the raw relative spelling is gone: {spec}"
    );
    let _ = fs::remove_dir_all(&box_.temp);
}

/// The already-declared refusal (niubash#176): for a stranded declaration
/// (declared, never installed) the hint is `niu plugin sync --prune` — the
/// suggested `niu plugin source remove` command would only fail. For an
/// installed source the remove hint stays.
#[test]
fn already_declared_hint_matches_the_state() {
    let box_ = Sandbox::new("dup-hint");
    let cwd = box_.temp.join("project");
    fs::create_dir_all(&cwd).unwrap();

    // A stranded declaration: hand-written spec entry, nothing installed.
    fs::write(
        box_.home.join(".niubash").join("plugins.toml"),
        "[[sources]]\ntarget = './ghost'\n",
    )
    .unwrap();
    let output = box_.run_in(&["plugin", "add", "./ghost"], &cwd);
    assert!(!output.status.success(), "duplicate must be refused");
    let stderr = stderr_text(&output);
    assert!(stderr.contains("already declared"), "{stderr}");
    assert!(
        stderr.contains("sync --prune"),
        "stranded entries point at sync --prune: {stderr}"
    );

    // An installed source keeps the remove hint.
    write_local_omb(&cwd.join("real"));
    let add = box_.run_in(&["plugin", "add", "./real"], &cwd);
    assert!(
        add.status.success(),
        "{} {}",
        stdout_text(&add),
        stderr_text(&add)
    );
    let dup = box_.run_in(&["plugin", "add", "./real"], &cwd);
    assert!(!dup.status.success());
    let stderr = stderr_text(&dup);
    assert!(stderr.contains("source remove"), "installed hint: {stderr}");
    let _ = fs::remove_dir_all(&box_.temp);
}
