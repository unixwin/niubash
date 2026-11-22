//! niubash#1061: Non-interactive `niu-git` / `git` invocations (no tty on
//! stdin) must not hang indefinitely waiting on credential prompts when no
//! credential helper supplies credentials. When stdin is not a terminal and
//! neither `GIT_TERMINAL_PROMPT` nor `GIT_ASKPASS` is set, niubash defaults
//! `GIT_TERMINAL_PROMPT=0` so git fails fast while still honoring configured
//! `credential.helper`s, explicit `GIT_TERMINAL_PROMPT=1`, `GIT_ASKPASS`, and
//! interactive TTY sessions.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use portable_pty::{native_pty_system, CommandBuilder, PtySize};

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

fn run_niu_noninteractive(cmd: &str, envs: &[(&str, &str)]) -> std::process::Output {
    let mut command = Command::new(niu_binary());
    command
        .args(["--norc", "--noprofile", "-c", cmd])
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .stdin(Stdio::null());
    if let Ok(system_root) = std::env::var("SystemRoot") {
        command.env("SystemRoot", system_root);
    }
    if let Ok(temp) = std::env::var("TEMP") {
        command.env("TEMP", temp);
    }
    for (name, value) in envs {
        command.env(name, value);
    }
    command.output().expect("spawn niu")
}

#[test]
fn noninteractive_piped_stdin_seeds_git_terminal_prompt_zero() {
    let output = run_niu_noninteractive("echo \"prompt=${GIT_TERMINAL_PROMPT:-unset}\"", &[]);
    assert!(
        output.status.success(),
        "niu failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("prompt=0"),
        "expected GIT_TERMINAL_PROMPT=0 when stdin is not a tty, got: {stdout:?}"
    );
}

#[test]
fn noninteractive_preserves_explicit_git_terminal_prompt() {
    let output = run_niu_noninteractive(
        "echo \"prompt=${GIT_TERMINAL_PROMPT:-unset}\"",
        &[("GIT_TERMINAL_PROMPT", "1")],
    );
    assert!(
        output.status.success(),
        "niu failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("prompt=1"),
        "expected explicit GIT_TERMINAL_PROMPT=1 to be preserved, got: {stdout:?}"
    );
}

#[test]
fn noninteractive_leaves_prompt_unset_when_git_askpass_is_set() {
    let output = run_niu_noninteractive(
        "echo \"prompt=${GIT_TERMINAL_PROMPT:-unset}\"",
        &[("GIT_ASKPASS", "echo")],
    );
    assert!(
        output.status.success(),
        "niu failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("prompt=unset"),
        "expected GIT_TERMINAL_PROMPT to remain unset when GIT_ASKPASS is set, got: {stdout:?}"
    );
}

#[test]
fn noninteractive_git_ls_remote_fails_fast_on_http_401() {
    let git_available = Command::new("git")
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
    if !git_available {
        return;
    }

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind local 401 server");
    let port = listener.local_addr().unwrap().port();
    let stop_server = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stop_bg = std::sync::Arc::clone(&stop_server);
    let _ = listener.set_nonblocking(true);
    let server = std::thread::spawn(move || {
        while !stop_bg.load(std::sync::atomic::Ordering::Relaxed) {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    let _ = stream.set_nonblocking(false);
                    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
                    let mut buf = [0u8; 1024];
                    let _ = stream.read(&mut buf);
                    let _ = stream.write_all(
                        b"HTTP/1.1 401 Unauthorized\r\nWWW-Authenticate: Basic realm=\"Test\"\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                    );
                    let _ = stream.flush();
                }
                Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(_) => break,
            }
        }
    });

    let null_config = if cfg!(windows) { "NUL" } else { "/dev/null" };
    let script = format!("git -c credential.helper= ls-remote http://127.0.0.1:{port}/repo.git");
    let mut command = Command::new(niu_binary());
    command
        .args(["--norc", "--noprofile", "-c", &script])
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("GIT_CONFIG_GLOBAL", null_config)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Ok(system_root) = std::env::var("SystemRoot") {
        command.env("SystemRoot", system_root);
    }
    if let Ok(temp) = std::env::var("TEMP") {
        command.env("TEMP", temp);
    }

    let mut child = command.spawn().expect("spawn niu git ls-remote");
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if child.try_wait().expect("poll child").is_some() {
            break;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("git ls-remote hung in non-interactive niu session");
        }
        std::thread::sleep(Duration::from_millis(50));
    }

    let output = child.wait_with_output().expect("collect output");
    stop_server.store(true, std::sync::atomic::Ordering::Relaxed);
    let _ = server.join();
    assert!(
        !output.status.success(),
        "expected git ls-remote against 401 to fail"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("terminal prompts disabled"),
        "expected fast-fail 'terminal prompts disabled' diagnostic, got stderr:\n{stderr}"
    );
}

#[test]
fn interactive_pty_does_not_seed_git_terminal_prompt_zero() {
    let pty_system = native_pty_system();
    let pair = pty_system
        .openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("open pty");

    let mut cmd = CommandBuilder::new(niu_binary());
    cmd.env_clear();
    if let Ok(system_root) = std::env::var("SystemRoot") {
        cmd.env("SystemRoot", system_root);
    }
    cmd.env("PATH", std::env::var("PATH").unwrap_or_default());
    if let Ok(temp) = std::env::var("TEMP") {
        cmd.env("TEMP", temp);
    }
    cmd.env("TERM", "xterm");
    cmd.env("NIU_LANG", "en");
    cmd.env("RUST_LOG", "off");
    cmd.env("RUST_BACKTRACE", "0");
    cmd.args([
        "--norc",
        "--noprofile",
        "-c",
        "echo \"prompt=${GIT_TERMINAL_PROMPT:-unset}\"",
    ]);

    let mut child = pair.slave.spawn_command(cmd).expect("spawn niu in pty");
    drop(pair.slave);

    let mut writer = pair.master.take_writer().expect("take pty writer");
    let mut reader = pair.master.try_clone_reader().expect("clone pty reader");
    let collected = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let collected_bg = std::sync::Arc::clone(&collected);
    let _pump = std::thread::spawn(move || {
        let mut buf = [0u8; 4096];
        while let Ok(n) = reader.read(&mut buf) {
            if n == 0 {
                break;
            }
            let chunk = &buf[..n];
            if chunk.windows(4).any(|w| w == b"\x1b[6n") {
                let _ = writer.write_all(b"\x1b[1;1R");
                let _ = writer.flush();
            }
            collected_bg.lock().unwrap().extend_from_slice(chunk);
        }
    });

    let deadline = Instant::now() + Duration::from_secs(10);
    let status = loop {
        if let Some(status) = child.try_wait().expect("poll pty child") {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            let partial = String::from_utf8_lossy(&collected.lock().unwrap()).into_owned();
            panic!("pty child hung; partial output: {partial:?}");
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    std::thread::sleep(Duration::from_millis(100));
    drop(pair.master);

    let output = String::from_utf8_lossy(&collected.lock().unwrap()).into_owned();

    assert!(status.success(), "pty child failed: {status:?}\n{output}");
    assert!(
        output.contains("prompt=unset"),
        "expected GIT_TERMINAL_PROMPT to stay unset when stdin is a TTY, got: {output:?}"
    );
}
