//! Mirror pipeline CLI tests (design §14.8, wt49/bmirror; git-only since
//! the download retraction 2026-10-04): `niu plugin mirror set <url>` is
//! the one-command configuration face — it validates and normalizes a
//! pasted mirror URL, round-trips through `~/.niubash/mirrors.toml`
//! (env-scoped for the test), and `mirror list` reports the active git
//! channel. The transport invariants (git insteadOf, canonical origins in
//! records, legacy download channels ignored) are pinned in the
//! `plugins::mirrors` unit tests and the sources.rs arg-builder test;
//! here we pin the user-facing control surface.
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

/// `set <url>` → normalized git_instead_of base on disk, `list` shows the
/// channel, `set none` returns to direct while preserving the section for
/// a one-line re-enable.
#[test]
fn mirror_set_url_roundtrip_and_disable() {
    let home = temp_dir("mirror-cli");
    let config = home.join("mirrors.toml");
    let envs = [("NIU_MIRRORS", config.to_string_lossy().into_owned())];

    // Direct by default.
    let out = run_niu_with_env(&["plugin", "mirror", "list"], &envs);
    assert_success(&out, "mirror list (default)");
    let text = stdout_text(&out);
    assert!(text.contains("direct connection"), "{text}");

    // set normalizes the missing trailing slash into the insteadOf base.
    let out = run_niu_with_env(
        &[
            "plugin",
            "mirror",
            "set",
            "https://mirror.example.com/github.com",
        ],
        &envs,
    );
    assert_success(&out, "mirror set url");
    let text = stdout_text(&out);
    assert!(
        text.contains("https://mirror.example.com/github.com/"),
        "{text}"
    );

    let written = fs::read_to_string(&config).unwrap();
    assert!(written.contains("niubash:mirrors@0.1.0"));
    assert!(written.contains("active = \"custom\""));
    assert!(written.contains("git_instead_of = \"https://mirror.example.com/github.com/\""));
    // The community-service caveat ships in the file, not as a preset.
    assert!(written.contains("UNSUPPORTED"));

    // list reports the active channel.
    let out = run_niu_with_env(&["plugin", "mirror", "list"], &envs);
    assert_success(&out, "mirror list (custom)");
    let text = stdout_text(&out);
    assert!(text.contains("custom mirror active"), "{text}");
    assert!(
        text.contains("https://mirror.example.com/github.com/"),
        "{text}"
    );
    assert!(text.contains("insteadOf"), "{text}");

    // set none goes back to direct but keeps the section.
    let out = run_niu_with_env(&["plugin", "mirror", "set", "none"], &envs);
    assert_success(&out, "mirror set none");
    let out = run_niu_with_env(&["plugin", "mirror", "list"], &envs);
    assert_success(&out, "mirror list (none)");
    let text = stdout_text(&out);
    assert!(text.contains("direct connection"), "{text}");
    let written = fs::read_to_string(&config).unwrap();
    assert!(
        written.contains("git_instead_of = \"https://mirror.example.com/github.com/\""),
        "custom section must survive set none:\n{written}"
    );

    let _ = fs::remove_dir_all(&home);
}

/// Garbage input is refused without touching an existing config, and the
/// help surface exists (the documented gate for this lane).
#[test]
fn mirror_set_rejects_garbage_and_help_exists() {
    let home = temp_dir("mirror-cli-reject");
    let config = home.join("mirrors.toml");
    let envs = [("NIU_MIRRORS", config.to_string_lossy().into_owned())];

    let out = run_niu_with_env(&["plugin", "mirror", "set", "not a url"], &envs);
    assert!(
        !out.status.success(),
        "garbage mirror url must be rejected: {}",
        stdout_text(&out)
    );
    assert!(!config.exists(), "rejected set must not create the file");

    let out = run_niu_with_env(&["plugin", "mirror", "--help"], &envs);
    assert_success(&out, "mirror --help");
    let text = stdout_text(&out);
    for needed in ["set <mirror-url>", "set none", "git_instead_of", "git-only"] {
        assert!(text.contains(needed), "help missing '{needed}':\n{text}");
    }
    // The retracted probe verb must not be offered anymore.
    assert!(
        !text.contains("  test "),
        "mirror test verb retired:\n{text}"
    );

    let out = run_niu_with_env(&["plugin", "--help"], &envs);
    assert_success(&out, "plugin --help");
    let text = stdout_text(&out);
    assert!(
        text.contains("mirror <command>"),
        "plugin help must list the mirror noun:\n{text}"
    );

    let _ = fs::remove_dir_all(&home);
}

/// A hand-edited config (git-only insteadOf mirror) is reported by `list`
/// with its channel — the file is a first-class editing surface.
#[test]
fn mirror_list_reports_hand_edited_git_only_config() {
    let home = temp_dir("mirror-cli-git");
    let config = home.join("mirrors.toml");
    fs::write(
        &config,
        concat!(
            "schema = \"niubash:mirrors@0.1.0\"\n",
            "active = \"custom\"\n",
            "\n[github]\n",
            "git_instead_of = \"https://git.example.com/github.com\"\n",
        ),
    )
    .unwrap();
    let envs = [("NIU_MIRRORS", config.to_string_lossy().into_owned())];

    let out = run_niu_with_env(&["plugin", "mirror", "list"], &envs);
    assert_success(&out, "mirror list (git-only)");
    let text = stdout_text(&out);
    assert!(text.contains("custom mirror active"), "{text}");
    assert!(
        text.contains("https://git.example.com/github.com"),
        "{text}"
    );
    assert!(text.contains("insteadOf"), "{text}");

    let _ = fs::remove_dir_all(&home);
}

/// niubash#298: a plaintext `http://` mirror ships every git fetch
/// unencrypted and unauthenticated — setting one is refused without the
/// explicit `--allow-plaintext` confirm, in interactive and scripted runs
/// alike, and the confirm still prints the standing warning.
#[test]
fn plaintext_http_mirror_requires_explicit_confirmation() {
    let home = temp_dir("mirror-cli-plaintext");
    let config = home.join("mirrors.toml");
    let envs = [("NIU_MIRRORS", config.to_string_lossy().into_owned())];

    // Without the flag: refused, nothing written.
    let refused = run_niu_with_env(
        &[
            "plugin",
            "mirror",
            "set",
            "http://mirror.example.com/github.com",
        ],
        &envs,
    );
    assert!(
        !refused.status.success(),
        "plaintext mirror must be refused: {}",
        stdout_text(&refused)
    );
    let err = String::from_utf8_lossy(&refused.stderr);
    assert!(err.contains("--allow-plaintext"), "{err}");
    assert!(!config.exists(), "the refused mirror must not be persisted");

    // With the flag: set, and the warning ships.
    let allowed = run_niu_with_env(
        &[
            "plugin",
            "mirror",
            "set",
            "http://mirror.example.com/github.com",
            "--allow-plaintext",
        ],
        &envs,
    );
    assert_success(&allowed, "mirror set --allow-plaintext");
    let text = stdout_text(&allowed);
    assert!(
        text.contains("http://mirror.example.com/github.com"),
        "{text}"
    );
    assert!(
        text.contains("unencrypted"),
        "the plaintext warning must ship: {text}"
    );
    let stored = fs::read_to_string(&config).unwrap();
    assert!(stored.contains("http://mirror.example.com/github.com"),);

    // An https:// mirror never needs the flag.
    let https = home.join("https.toml");
    let envs2 = [("NIU_MIRRORS", https.to_string_lossy().into_owned())];
    let set = run_niu_with_env(
        &[
            "plugin",
            "mirror",
            "set",
            "https://gitclone.example.com/github.com",
        ],
        &envs2,
    );
    assert_success(&set, "https mirror set needs no confirm");

    let _ = fs::remove_dir_all(&home);
}
