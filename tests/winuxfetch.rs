use std::path::PathBuf;
use std::process::Command;

fn winuxfetch_binary() -> PathBuf {
    let p = PathBuf::from(env!("CARGO_BIN_EXE_winuxfetch"));
    if p.exists() {
        return p;
    }

    let mut fallback = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    fallback.push("target");
    fallback.push("debug");
    fallback.push(if cfg!(windows) {
        "winuxfetch.exe"
    } else {
        "winuxfetch"
    });
    fallback
}

#[test]
fn winuxfetch_no_logo_prints_core_fields() {
    let output = Command::new(winuxfetch_binary())
        .args(["--no-logo", "--no-color"])
        .output()
        .expect("failed to run winuxfetch");

    assert!(
        output.status.success(),
        "winuxfetch failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("OS:"), "{stdout}");
    assert!(stdout.contains("Shell: winuxsh"), "{stdout}");
    assert!(stdout.contains("Terminal:"), "{stdout}");
}

#[test]
fn winuxfetch_license_mentions_neofetch() {
    let output = Command::new(winuxfetch_binary())
        .arg("--license")
        .output()
        .expect("failed to run winuxfetch --license");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Neofetch"), "{stdout}");
    assert!(stdout.contains("Dylan Araps"), "{stdout}");
}
