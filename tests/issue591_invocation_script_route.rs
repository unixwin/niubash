//! niubash#591: the ShellInvocation script route (`niu -- script`)
//! shares the direct route's script-opening logic — $PATH search for a
//! slash-less name, and directory / permission / binary refusals with
//! GNU's diagnostics and statuses (127 not found, 126 not
//! executable). It used to read the operand directly: no PATH search
//! (rc 127 for a findable script) and a real ELF parsed as script
//! text (rc 127 instead of 126).

use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

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

struct Fixture {
    base: PathBuf,
    home: PathBuf,
    work: PathBuf,
    bin: PathBuf,
}

impl Fixture {
    fn new(label: &str) -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let base = std::env::temp_dir().join(format!(
            "niu-issue591-{label}-{}-{nanos}",
            std::process::id()
        ));
        let home = base.join("home");
        let work = base.join("work");
        let bin = base.join("bin");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&work).unwrap();
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(bin.join("myscript"), "echo SCRIPT-RAN\n").unwrap();
        // A real ELF image (magic + payload + NULs), not text that
        // merely mentions ELF — the original audit fixture wrote the
        // literal characters and proved nothing.
        let mut elf = vec![0x7f, b'E', b'L', b'F', 0x02, 0x01, 0x01, 0x00];
        elf.extend_from_slice(&[0x00; 8]);
        elf.extend_from_slice(b"garbage\x00tail");
        std::fs::write(work.join("elfbin"), elf).unwrap();
        std::fs::create_dir_all(work.join("somedir")).unwrap();
        Self {
            base,
            home,
            work,
            bin,
        }
    }

    fn run(&self, args: &[&str]) -> Output {
        let path = format!(
            "{}:{}",
            self.bin.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        Command::new(niu_binary())
            .args(args)
            .current_dir(&self.work)
            .env_clear()
            .env("PATH", path)
            .env("HOME", &self.home)
            .env("USERPROFILE", &self.home)
            .stdin(Stdio::null())
            .output()
            .expect("spawn niu")
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

fn stdout_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn invocation_route_finds_the_script_on_path() {
    let fixture = Fixture::new("path");
    let direct = fixture.run(&["myscript"]);
    assert!(direct.status.success(), "{}", stderr_of(&direct));
    assert!(stdout_of(&direct).contains("SCRIPT-RAN"));

    let invocation = fixture.run(&["--", "myscript"]);
    assert!(
        invocation.status.success(),
        "niu -- myscript must search PATH like the direct route: {}",
        stderr_of(&invocation)
    );
    assert!(
        stdout_of(&invocation).contains("SCRIPT-RAN"),
        "{}",
        stdout_of(&invocation)
    );
}

#[test]
fn invocation_route_refuses_a_binary_with_126() {
    let fixture = Fixture::new("binary");
    let direct = fixture.run(&["elfbin"]);
    assert_eq!(direct.status.code(), Some(126), "{}", stderr_of(&direct));
    assert!(stderr_of(&direct).contains("cannot execute binary file"));

    let invocation = fixture.run(&["--", "elfbin"]);
    assert_eq!(
        invocation.status.code(),
        Some(126),
        "an ELF operand is refused, not parsed as script text: {}",
        stderr_of(&invocation)
    );
    assert!(
        stderr_of(&invocation).contains("cannot execute binary file"),
        "{}",
        stderr_of(&invocation)
    );
}

#[test]
fn invocation_route_missing_script_is_127() {
    let fixture = Fixture::new("missing");
    let invocation = fixture.run(&["--", "nosuch.sh"]);
    assert_eq!(invocation.status.code(), Some(127));
    assert!(
        stderr_of(&invocation).contains("No such file or directory"),
        "{}",
        stderr_of(&invocation)
    );
}

#[test]
fn invocation_route_directory_operand_is_126() {
    let fixture = Fixture::new("dir");
    let invocation = fixture.run(&["--", "somedir"]);
    assert_eq!(invocation.status.code(), Some(126));
    assert!(
        stderr_of(&invocation).contains("Is a directory"),
        "{}",
        stderr_of(&invocation)
    );
}

#[test]
fn invocation_route_still_runs_a_plain_script() {
    let fixture = Fixture::new("plain");
    std::fs::write(fixture.work.join("plain.sh"), "echo PLAIN-RAN\n").unwrap();
    let invocation = fixture.run(&["--", "plain.sh"]);
    assert!(invocation.status.success(), "{}", stderr_of(&invocation));
    assert!(stdout_of(&invocation).contains("PLAIN-RAN"));
    // Positional parameters still flow after the script operand.
    std::fs::write(fixture.work.join("args.sh"), "echo \"got=$1\"\n").unwrap();
    let with_args = fixture.run(&["--", "args.sh", "hello"]);
    assert!(stdout_of(&with_args).contains("got=hello"));
}
