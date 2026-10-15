use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

struct Sandbox(PathBuf);

impl Sandbox {
    fn new() -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root =
            std::env::temp_dir().join(format!("niu-bootstrap-{}-{nanos}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        Self(root)
    }

    fn run(&self, args: &[&str]) -> std::process::Output {
        Command::new(env!("CARGO_BIN_EXE_niu"))
            .args(["plugin", "sync"])
            .args(args)
            .current_dir(&self.0)
            .env("HOME", &self.0)
            .env("USERPROFILE", &self.0)
            .env("NIU_PLUGIN_SPEC", self.0.join("plugins.toml"))
            .env("NIU_PLUGIN_SOURCES_ROOT", self.0.join("sources"))
            .env_remove("NIU_PLUGIN_BOOTSTRAP")
            .output()
            .unwrap()
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn bootstrap_leaves_empty_home_empty() {
    let sandbox = Sandbox::new();
    let output = sandbox.run(&["--bootstrap"]);
    assert!(output.status.success(), "{output:?}");
    assert!(output.stdout.is_empty(), "{output:?}");
    assert!(output.stderr.is_empty(), "{output:?}");
    assert_eq!(fs::read_dir(&sandbox.0).unwrap().count(), 0);
}

#[test]
fn bootstrap_preserves_broken_configuration() {
    let sandbox = Sandbox::new();
    let files = [
        (".niubashrc", "if true; then\r\n"),
        ("plugins.toml", "[invalid TOML\n"),
        (".niubash/sources.lock", "invalid lock\n"),
        ("sources/existing/plugin.sh", "echo keep\n"),
    ];
    for (name, contents) in &files {
        let path = sandbox.0.join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }
    for options in [
        vec!["--bootstrap"],
        vec!["--bootstrap", "--prune", "--adopt", "--timeout", "1"],
        vec!["--prune", "--adopt", "--timeout=1", "--bootstrap"],
    ] {
        let output = sandbox.run(&options);
        assert!(output.status.success(), "{output:?}");
        assert!(output.stdout.is_empty(), "{output:?}");
        assert!(output.stderr.is_empty(), "{output:?}");
        for (name, contents) in &files {
            assert_eq!(fs::read(sandbox.0.join(name)).unwrap(), contents.as_bytes());
        }
        assert_eq!(fs::read_dir(sandbox.0.join(".niubash")).unwrap().count(), 1);
        assert_eq!(fs::read_dir(sandbox.0.join("sources")).unwrap().count(), 1);
        assert_eq!(fs::read_dir(&sandbox.0).unwrap().count(), 4);
    }
}

#[test]
fn explicit_sync_still_validates_the_spec() {
    let sandbox = Sandbox::new();
    fs::write(sandbox.0.join("plugins.toml"), "[invalid TOML\n").unwrap();
    let output = sandbox.run(&[]);
    assert!(!output.status.success(), "{output:?}");
    assert!(!output.stderr.is_empty(), "{output:?}");
}
