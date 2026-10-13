//! Integration: the theme-preview child channel (niubash#170).
//!
//! `plugins::theme_preview::render_theme` spawns a throwaway
//! `niu -c` child that sources the theme's managed block and prints the
//! rendered PS1 between markers. These cases run the REAL pipeline — the
//! real niu binary (via `NIU_PREVIEW_NIU_EXE`), the real fixture loader
//! tree, the real engine expansion — against a sandboxed registry:
//!
//! - a working theme renders its face (through the interactive marker the
//!   gate sets, because the fixture/real oh-my-bash loader bails on
//!   `case $- in *i*)` when `$-` lacks `i`);
//! - a theme that never sets PS1 degrades with a reason, not a hang;
//! - a hung theme is killed at the render timeout and degrades;
//! - PS1 backslash escapes are expanded by the ENGINE in the child (the
//!   parent only strips the non-printing `\[`/`\]` markers).

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use niubash_runtime::plugins::sources::{self, SourceInstallRequest};
use niubash_runtime::plugins::theme_preview::{
    preview_lines, render_theme, PreviewOutcome, ThemePreviewRequest, RENDER_TIMEOUT,
};

fn unique_temp_dir(prefix: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("{prefix}-{}-{nanos}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn host_to_shell_style_path(path: &Path) -> String {
    let display = path.to_string_lossy().replace('\\', "/");
    if cfg!(windows) && display.len() >= 3 && display.as_bytes()[1] == b':' {
        let drive = display.as_bytes()[0] as char;
        let drive = drive.to_ascii_lowercase();
        format!("/{drive}/{}", &display[3..])
    } else {
        display
    }
}

struct EnvGuard {
    name: &'static str,
    previous: Option<std::ffi::OsString>,
}

impl EnvGuard {
    fn set(name: &'static str, value: &str) -> Self {
        let previous = std::env::var_os(name);
        std::env::set_var(name, value);
        Self { name, previous }
    }

    fn unset(name: &'static str) -> Self {
        let previous = std::env::var_os(name);
        std::env::remove_var(name);
        Self { name, previous }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        if let Some(previous) = &self.previous {
            std::env::set_var(self.name, previous);
        } else {
            std::env::remove_var(self.name);
        }
    }
}

/// A sandbox with the fixture oh-my-bash tree installed and trusted, plus
/// the representative preview themes written into the installed copy.
struct PreviewSandbox {
    _guards: Vec<EnvGuard>,
    sources_root: PathBuf,
    #[allow(dead_code)] // kept for parity with the other sandbox fixtures
    home: PathBuf,
}

impl PreviewSandbox {
    fn new(label: &str) -> Self {
        let temp = unique_temp_dir(label);
        let home = temp.join("home");
        let sources_root = temp.join("sources");
        let _guards = vec![
            EnvGuard::set("HOME", &host_to_shell_style_path(&home)),
            EnvGuard::unset("USERPROFILE"),
            EnvGuard::set("NIU_PLUGIN_SOURCES_ROOT", &sources_root.to_string_lossy()),
            // The preview children must run the freshly built niu, not this
            // test harness (what `current_exe` would resolve to).
            EnvGuard::set("NIU_PREVIEW_NIU_EXE", env!("CARGO_BIN_EXE_niu")),
        ];

        // Stage the fixture tree OUTSIDE the sources root: `add_source`
        // copies local origins into the root itself and refuses an existing
        // target directory.
        let staged = temp.join("staged-oh-my-bash");
        fs::create_dir_all(&staged).unwrap();
        copy_dir(
            &PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sources/oh-my-bash"),
            &staged,
        );
        // Representative preview themes (niubash#170 classes) written into
        // the staged copy — the repo fixture stays untouched.
        write_theme(
            &staged,
            "hung",
            "# never sets PS1 and never finishes\nsleep 30\n",
        );
        write_theme(&staged, "noprompt", "# loads fine, sets no prompt\ntrue\n");
        write_theme(
            &staged,
            "colored",
            "# PS1 escapes the engine must expand in the child\n\
             PS1='\\[\\e[32m\\]\\u@\\h\\[\\e[0m\\]:\\w\\n\\$ '\n",
        );

        sources::add_source(SourceInstallRequest {
            adapter: None,
            origin: staged.to_string_lossy().into_owned(),
            ref_name: None,
            commit: None,
            expected_checksum: None,
            id: None,
            entry: None,
            fetch_budget: None,
            credential_guard: false,
        })
        .expect("fixture source add must succeed");
        sources::trust_source("oh-my-bash").expect("fixture trust must succeed");

        PreviewSandbox {
            _guards,
            sources_root,
            home,
        }
    }
}

impl Drop for PreviewSandbox {
    fn drop(&mut self) {
        let _ = sources::remove_source("oh-my-bash");
        if let Some(parent) = self.sources_root.parent() {
            let _ = fs::remove_dir_all(parent);
        }
    }
}

fn copy_dir(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.path().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), &target).unwrap();
        }
    }
}

fn write_theme(installed: &Path, name: &str, body: &str) {
    let dir = installed.join("themes").join(name);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join(format!("{name}.theme.sh")), body).unwrap();
}

fn request(name: &str) -> ThemePreviewRequest {
    ThemePreviewRequest {
        source_id: "oh-my-bash".to_string(),
        name: name.to_string(),
    }
}

/// A working theme renders its real face: the fixture theme composes PS1
/// through the loader (`➜ prompt-robbyrussell `), so a preview proves the
/// whole channel — managed block → interactive-marker sourcing → engine
/// expansion → marker extraction.
#[test]
fn preview_renders_the_theme_face_through_the_child_channel() {
    let _sandbox = PreviewSandbox::new("theme-preview-face");
    let outcome = render_theme(&request("robbyrussell"));
    let PreviewOutcome::Rendered(lines) = outcome else {
        panic!("expected a rendered preview, got {outcome:?}");
    };
    let joined = lines.join("\n");
    assert!(
        joined.contains("prompt-robbyrussell"),
        "the theme's PS1 text must render: {lines:?}"
    );
    assert!(
        joined.contains('➜'),
        "the theme's own glyph must survive byte-for-byte: {joined:?}"
    );
    assert!(
        !joined.contains("__NIU_PS1"),
        "markers must be stripped: {joined:?}"
    );
}

/// The engine expands PS1 backslash escapes in the child; the parent only
/// strips the non-printing ignore markers and keeps colors.
#[test]
fn preview_expands_ps1_escapes_in_the_child() {
    let _sandbox = PreviewSandbox::new("theme-preview-escapes");
    let outcome = render_theme(&request("colored"));
    let PreviewOutcome::Rendered(lines) = outcome else {
        panic!("expected a rendered preview, got {outcome:?}");
    };
    assert_eq!(lines.len(), 2, "the theme PS1 is two-line: {lines:?}");
    let first = &lines[0];
    assert!(first.contains("\x1b[32m"), "colors kept: {first:?}");
    assert!(first.contains('@'), "user@host escapes expanded: {first:?}");
    assert!(first.contains(':'), "the cwd separator expanded: {first:?}");
    assert!(!first.contains('\\'), "no raw escapes left: {first:?}");
    assert!(!first.contains('\x01') && !first.contains('\x02'));
    // GNU parse.y: `\$` renders `#` for the privileged user, `$` otherwise —
    // this host runs as Administrator, so accept exactly the engine's two
    // legal faces and nothing else.
    assert!(
        lines[1] == "# " || lines[1] == "$ ",
        "second line is the dollar face: {lines:?}"
    );
}

/// A theme that loads but sets no PS1 degrades with a reason — never a
/// blank preview presented as success.
#[test]
fn theme_without_prompt_degrades_to_the_unavailable_note() {
    let _sandbox = PreviewSandbox::new("theme-preview-noprompt");
    let outcome = render_theme(&request("noprompt"));
    assert_eq!(
        outcome,
        PreviewOutcome::Unavailable("theme set no prompt".to_string()),
        "{outcome:?}"
    );
    assert_eq!(
        outcome.lines(),
        vec!["(preview unavailable: theme set no prompt)".to_string()]
    );
}

/// A hung theme is killed at the render timeout and degrades — the gallery
/// must never freeze on it.
#[test]
fn hung_theme_is_killed_at_the_render_timeout() {
    let _sandbox = PreviewSandbox::new("theme-preview-hang");
    let started = Instant::now();
    let outcome = render_theme(&request("hung"));
    let elapsed = started.elapsed();
    assert!(
        matches!(outcome, PreviewOutcome::Unavailable(ref reason) if reason.contains("timed out")),
        "expected the timeout degradation, got {outcome:?}"
    );
    assert!(
        elapsed >= RENDER_TIMEOUT.saturating_sub(Duration::from_millis(200)),
        "the bound must actually wait for the child, took {elapsed:?}"
    );
    assert!(
        elapsed < Duration::from_secs(6),
        "the child must be killed near the 1.5s bound (theme sleeps 30s), took {elapsed:?}"
    );
}

/// Unknown/untrusted sources degrade with a nameable reason before any
/// child is spawned.
#[test]
fn unknown_source_degrades_without_spawning() {
    let _sandbox = PreviewSandbox::new("theme-preview-unknown");
    let outcome = render_theme(&ThemePreviewRequest {
        source_id: "no-such-source".to_string(),
        name: "x".to_string(),
    });
    let PreviewOutcome::Unavailable(reason) = outcome else {
        panic!("expected degradation, got {outcome:?}");
    };
    assert!(reason.contains("no-such-source"), "{reason}");
}

/// The pane-line shaping: marker bytes and literal `\[`/`\]` never reach the
/// pane, colors survive, over-tall prompts cap with the ellipsis tail.
#[test]
fn preview_lines_shape_the_pane_content() {
    let lines = preview_lines("\x01\x1b[36m\x02top\n\x1b[0mbottom ");
    assert_eq!(lines, vec!["\x1b[36mtop", "\x1b[0mbottom "]);
    let tall = preview_lines("1\n2\n3\n4\n5\n");
    assert_eq!(tall.len(), 4);
    assert!(tall[3].ends_with('…'), "{tall:?}");
}
