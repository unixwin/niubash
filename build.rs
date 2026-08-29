use std::fs;

fn main() {
    emit_rubash_revision();

    #[cfg(windows)]
    embed_windows_icon();
}

fn emit_rubash_revision() {
    println!("cargo:rerun-if-changed=Cargo.lock");

    let revision = fs::read_to_string("Cargo.lock")
        .ok()
        .and_then(|lock| rubash_revision_from_lock(&lock))
        .unwrap_or_else(|| "master".to_string());

    println!("cargo:rustc-env=NIU_RUBASH_REV={revision}");
}

fn rubash_revision_from_lock(lock: &str) -> Option<String> {
    let mut in_rubash = false;

    for line in lock.lines() {
        let trimmed = line.trim();
        if trimmed == "[[package]]" {
            in_rubash = false;
            continue;
        }

        if trimmed == "name = \"rubash\"" {
            in_rubash = true;
            continue;
        }

        if in_rubash && trimmed.starts_with("source = ") {
            let source = trimmed.trim_start_matches("source = ").trim_matches('"');
            return source
                .rsplit_once('#')
                .map(|(_, rev)| rev.to_string())
                .or_else(|| Some("master".to_string()));
        }
    }

    None
}

#[cfg(windows)]
fn embed_windows_icon() {
    use std::env;
    use std::path::{Path, PathBuf};
    use std::process::Command;

    let manifest_dir =
        PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let assets_dir = manifest_dir.join("assets");
    let rc_file = assets_dir.join("niubash.rc");
    let icon_file = assets_dir.join("niubash-icon.ico");
    let out_file = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR")).join("niubash.res");

    println!("cargo:rerun-if-changed={}", rc_file.display());
    println!("cargo:rerun-if-changed={}", icon_file.display());

    let rc_exe = find_resource_compiler().unwrap_or_else(|| {
        panic!(
            "could not find rc.exe or llvm-rc.exe; install the Windows SDK or LLVM resource compiler to embed niu.exe icon"
        )
    });

    let status = Command::new(&rc_exe)
        .current_dir(&assets_dir)
        .arg("/nologo")
        .arg(format!("/fo{}", out_file.display()))
        .arg(rc_file.file_name().expect("resource file name"))
        .status()
        .unwrap_or_else(|err| panic!("failed to run {}: {err}", rc_exe.display()));

    if !status.success() {
        panic!("{} failed with status {status}", rc_exe.display());
    }

    println!("cargo:rustc-link-arg-bin=niu={}", out_file.display());
    println!("cargo:rustc-link-arg-bin=niubash={}", out_file.display());

    fn find_resource_compiler() -> Option<PathBuf> {
        find_in_path("rc.exe")
            .or_else(|| find_in_path("llvm-rc.exe"))
            .or_else(find_windows_sdk_rc)
    }

    fn find_in_path(exe: &str) -> Option<PathBuf> {
        let path = env::var_os("PATH")?;
        env::split_paths(&path)
            .map(|dir| dir.join(exe))
            .find(|candidate| candidate.is_file())
    }

    fn find_windows_sdk_rc() -> Option<PathBuf> {
        let arch_dir = match env::var("TARGET").ok()?.as_str() {
            target if target.contains("aarch64") => "arm64",
            target if target.contains("i686") => "x86",
            _ => "x64",
        };

        let mut candidates = Vec::new();
        for root_var in ["ProgramFiles(x86)", "ProgramFiles"] {
            let Some(root) = env::var_os(root_var) else {
                continue;
            };
            let bin_dir = Path::new(&root).join("Windows Kits").join("10").join("bin");
            let Ok(entries) = fs::read_dir(bin_dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let candidate = entry.path().join(arch_dir).join("rc.exe");
                if candidate.is_file() {
                    candidates.push(candidate);
                }
            }
        }

        candidates.sort();
        candidates.pop()
    }
}
