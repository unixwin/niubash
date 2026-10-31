//! Host-wiring smoke tests for the native debugger faces: the `niu --dap`
//! route must speak framed DAP on stdio and propagate the debuggee's exit
//! status, and the bundled zero-JS VS Code manifest must launch `niu`.
//!
//! The engine's own debugger suites live in rubash; these stay deliberately
//! thin and assert the niubash-specific join points (argv dispatch, process
//! stdio, manifest identity) that no rubash test can cover.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

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

fn frame(body: &str) -> String {
    format!("Content-Length: {}\r\n\r\n{}", body.len(), body)
}

fn json_path(path: &Path) -> String {
    let text = path.to_string_lossy().replace('\\', "\\\\");
    format!("\"{}\"", text.replace('"', "\\\""))
}

#[test]
fn dap_route_serves_the_protocol_and_propagates_the_exit_status() {
    let dir = std::env::temp_dir().join(format!(
        "niubash-dap-smoke-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let script = dir.join("smoke.sh");
    std::fs::write(&script, "echo smoke\nexit 0\n").expect("write script");

    let mut child = Command::new(niu_binary())
        .arg("--dap")
        .arg(&script)
        .env_remove("NIU_INVOKED_AS")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn niu --dap");

    {
        let stdin = child.stdin.as_mut().expect("piped stdin");
        let launch = format!(
            "{{\"seq\":2,\"type\":\"request\",\"command\":\"launch\",\"arguments\":{{\"program\":{},\"stopOnEntry\":false}}}}",
            json_path(&script)
        );
        for body in [
            "{\"seq\":1,\"type\":\"request\",\"command\":\"initialize\",\"arguments\":{\"clientID\":\"niubash-test\"}}".to_string(),
            launch,
            "{\"seq\":3,\"type\":\"request\",\"command\":\"configurationDone\",\"arguments\":{}}".to_string(),
        ] {
            stdin
                .write_all(frame(&body).as_bytes())
                .expect("write request");
        }
        stdin.flush().expect("flush requests");
    }
    // EOF is the client leaving: the adapter runs the debuggee, reports the
    // terminal events, and exits through its normal epilogue.
    drop(child.stdin.take());

    let mut stdout = child.stdout.take().expect("piped stdout");
    let reader = std::thread::spawn(move || {
        let mut text = String::new();
        stdout
            .read_to_string(&mut text)
            .expect("read adapter stdout");
        text
    });

    let started = Instant::now();
    loop {
        match child.try_wait().expect("poll adapter") {
            Some(status) => {
                assert_eq!(
                    status.code(),
                    Some(0),
                    "the DAP route must exit with the debuggee's status"
                );
                break;
            }
            None if started.elapsed() > Duration::from_secs(20) => {
                let _ = child.kill();
                panic!("niu --dap hung; no exit after 20s");
            }
            None => std::thread::sleep(Duration::from_millis(20)),
        }
    }

    let output = reader.join().expect("reader thread");
    assert!(
        output.contains("\"command\":\"initialize\""),
        "initialize response missing: {output}"
    );
    assert!(
        output.contains("\"event\":\"exited\""),
        "exited event missing: {output}"
    );
    assert!(
        output.contains("\"exitCode\":0"),
        "debuggee exit status not reported: {output}"
    );
    assert!(
        output.contains("\"event\":\"terminated\""),
        "terminated event missing: {output}"
    );

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn vscode_manifest_launches_the_niubash_frontend() {
    let text = include_str!("../editors/vscode/package.json");
    let manifest: serde_json::Value = serde_json::from_str(text).expect("manifest parses");
    let debugger = &manifest["contributes"]["debuggers"][0];
    assert_eq!(debugger["type"], "niubash");
    assert_eq!(debugger["program"], "niu");
    let args: Vec<&str> = debugger["args"]
        .as_array()
        .expect("adapter args")
        .iter()
        .filter_map(serde_json::Value::as_str)
        .collect();
    assert_eq!(args, vec!["--debug", "--dap"]);
    assert!(
        manifest.get("main").is_none(),
        "a zero-JS extension has no activation entry point"
    );
}
