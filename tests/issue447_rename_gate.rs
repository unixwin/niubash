//! niubash#447 regression: the brand rename gate
//! (`scripts/check-rename-clean.sh`) used to fail on a pristine master
//! because its residue baseline was stale — every new change saw a
//! false red unrelated to its diff. The acceptance in #447: the gate is
//! green on a clean tree, and goes red only for genuinely new residue.
//! This test runs the gate itself; it skips when no `sh` is available to
//! the test process (the gate's documented runners are Git Bash and the
//! shell's own `-c` mode).

use std::path::PathBuf;
use std::process::Command;

#[test]
fn issue447_rename_gate_is_green_on_the_current_tree() {
    let Ok(sh) = which_sh() else { return };
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let output = Command::new(sh)
        .arg("scripts/check-rename-clean.sh")
        .current_dir(&root)
        .output()
        .expect("spawn rename gate");
    assert!(
        output.status.success(),
        "rename gate failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn which_sh() -> Result<PathBuf, ()> {
    let path = std::env::var_os("PATH").ok_or(())?;
    for dir in std::env::split_paths(&path) {
        for name in ["sh", "sh.exe"] {
            let candidate = dir.join(name);
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
    }
    Err(())
}
